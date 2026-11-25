//! The plain C ABI of the iOS share extension (#2100). Declared in
//! `pollis-core/include/pollis_share.h`.
//!
//! Every function is blocking: call it off the main thread. None returns NULL; strings are JSON,
//! owned by Rust, freed with `pollis_string_free`. Never message content in a log line.

use std::ffi::{c_char, c_void, CStr, CString};
use std::time::Duration;

use super::session::{self, Factor, Params};

const INTERNAL: &str = r#"{"ok":false,"reason":"internal"}"#;

unsafe fn opt_str(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok().map(str::to_string)
}

fn to_c(s: String) -> *mut c_char {
    CString::new(s)
        .unwrap_or_else(|_| c"{\"ok\":false,\"reason\":\"internal\"}".to_owned())
        .into_raw()
}

/// Run `f` on the core's big-stack workers (an extension thread's stack is too small for
/// SQLCipher and MLS) and turn a panic into the internal error.
fn on_worker<F, Fut>(f: F) -> String
where
    F: FnOnce() -> Fut + std::panic::UnwindSafe,
    Fut: std::future::Future<Output = String> + Send + 'static,
{
    std::panic::catch_unwind(move || {
        let rt = crate::bridge::worker_runtime();
        rt.block_on(rt.spawn(f())).unwrap_or_else(|_| INTERNAL.to_string())
    })
    .unwrap_or_else(|_| INTERNAL.to_string())
}

/// Start a session for whoever is signed in. `{"ok":true,"state":"signed_out"|"locked_out"}`
/// or `{"ok":true,"state":"locked","biometric":bool,"attempts_remaining":n}`.
///
/// # Safety
/// `data_dir` must be NULL or a valid NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn pollis_share_open(data_dir: *const c_char) -> *mut c_char {
    let data_dir = opt_str(data_dir).filter(|d| !d.is_empty());
    to_c(on_worker(move || async move {
        let Some(data_dir) = data_dir.map(std::path::PathBuf::from) else {
            return INTERNAL.to_string();
        };
        crate::notify::host::adopt_data_dir(&data_dir);
        crate::diag::ensure_init(&crate::diag::log_dir(&data_dir));
        // An extension never mints a keystore key (`keystore::forbid_key_creation`).
        crate::keystore::forbid_key_creation();
        let Some(config) = crate::notify::host::load_config(&data_dir) else {
            return ok_with(&session::Status::SignedOut);
        };
        let status = session::open(Params {
            keystore: crate::keystore::default_os_keystore(),
            config,
            data_dir,
        })
        .await;
        crate::diag!("[share] open: {}", serde_json::to_value(&status).map(|v| v["state"].to_string()).unwrap_or_default());
        ok_with(&status)
    }))
}

fn ok_with<T: serde::Serialize>(v: &T) -> String {
    let mut value = serde_json::to_value(v).unwrap_or_default();
    if let Some(map) = value.as_object_mut() {
        map.insert("ok".into(), serde_json::Value::Bool(true));
    }
    value.to_string()
}

fn unlock_json(factor: Factor) -> String {
    on_worker(move || async move {
        let outcome = session::unlock(factor).await;
        crate::diag!("[share] unlock: {}", serde_json::to_value(&outcome).map(|v| v["result"].to_string()).unwrap_or_default());
        ok_with(&outcome)
    })
}

/// Unlock with the PIN. `{"ok":true,"result":"unlocked"|"incorrect"|"locked_out"|"open_app"|
/// "no_session", "attempts_remaining":n}` (the count only with `incorrect`).
///
/// # Safety
/// `pin` must be NULL or a valid NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn pollis_share_unlock_pin(pin: *const c_char) -> *mut c_char {
    let pin = opt_str(pin).unwrap_or_default();
    to_c(unlock_json(Factor::Pin(pin)))
}

/// Unlock with the device unlock key read from the biometric-bound keychain item (base64).
/// As [`pollis_share_unlock_pin`], plus `"biometric_failed"`; never spends a PIN attempt.
///
/// # Safety
/// `device_key` must be NULL or a valid NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn pollis_share_unlock_device_key(device_key: *const c_char) -> *mut c_char {
    let key = opt_str(device_key).unwrap_or_default();
    to_c(unlock_json(Factor::DeviceKey(key)))
}

/// The picker's rows, best first. `{"ok":true,"targets":[…]}` or
/// `{"ok":false,"reason":"locked|network|internal"}`.
///
/// # Safety
/// `query` must be NULL or a valid NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn pollis_share_targets(query: *const c_char, limit: u32) -> *mut c_char {
    let query = opt_str(query).filter(|q| !q.trim().is_empty());
    to_c(on_worker(move || async move {
        let limit = (limit > 0).then_some(limit);
        match session::targets(query, limit).await {
            Ok(targets) => serde_json::json!({ "ok": true, "targets": targets }).to_string(),
            Err(e) => failure(e),
        }
    }))
}

fn failure(e: crate::error::Error) -> String {
    let text = e.to_string();
    let reason = if text.contains("share session") {
        "locked"
    } else {
        match crate::notify::fetch::classify(e) {
            crate::notify::fetch::Reason::Network => "network",
            _ => "internal",
        }
    };
    serde_json::json!({ "ok": false, "reason": reason }).to_string()
}

/// A progress callback: `ctx` as passed, and a JSON event valid only during the call.
pub type ProgressFn = unsafe extern "C" fn(ctx: *mut c_void, event: *const c_char);

/// The host's callback and its context, carried to the worker thread.
struct Progress {
    f: Option<ProgressFn>,
    ctx: *mut c_void,
}

// SAFETY: the header requires the callback and its context to be callable from any thread.
unsafe impl Send for Progress {}
unsafe impl Sync for Progress {}

impl Progress {
    fn emit(&self, event: &serde_json::Value) {
        let Some(f) = self.f else {
            return;
        };
        if let Ok(s) = CString::new(event.to_string()) {
            // SAFETY: the host's callback, called with its own context and a valid string.
            unsafe { f(self.ctx, s.as_ptr()) };
        }
    }
}

/// Store and deliver a share (`ShareRequest` JSON). Calls `progress` with
/// `{"event":"stored","queued":[…]}` once every target's message is stored, then
/// `{"event":"target","conversation_id","message_id","status":"sent|queued|rejected"}` per
/// target. Returns `{"ok":true,"results":[…]}`, or `{"ok":false,"reason":…}` when nothing could
/// be stored (`"invalid"` for a request the rules refuse).
///
/// # Safety
/// `request_json` must be NULL or a valid NUL-terminated string. `progress` may be NULL; if not,
/// it and `ctx` must be callable from any thread until this returns.
#[no_mangle]
pub unsafe extern "C" fn pollis_share_send(
    request_json: *const c_char,
    progress: Option<ProgressFn>,
    ctx: *mut c_void,
) -> *mut c_char {
    let request = opt_str(request_json).unwrap_or_default();
    let progress = std::sync::Arc::new(Progress { f: progress, ctx });
    let started = std::time::Instant::now();
    let out = on_worker(std::panic::AssertUnwindSafe(move || async move {
        let req: super::send::ShareRequest = match serde_json::from_str(&request) {
            Ok(r) => r,
            Err(_) => return r#"{"ok":false,"reason":"invalid"}"#.to_string(),
        };
        if let Err(e) = super::send::validate(&req) {
            crate::diag!("[share] request refused: {e}");
            return r#"{"ok":false,"reason":"invalid"}"#.to_string();
        }
        let stored = std::sync::Arc::clone(&progress);
        let each = std::sync::Arc::clone(&progress);
        let result = session::send(
            req,
            move |queued| stored.emit(&serde_json::json!({ "event": "stored", "queued": queued })),
            move |outcome| {
                let mut v = serde_json::to_value(outcome).unwrap_or_default();
                v["event"] = "target".into();
                each.emit(&v);
            },
        )
        .await;
        match result {
            Ok(results) => serde_json::json!({ "ok": true, "results": results }).to_string(),
            Err(e) => failure(e),
        }
    }));
    crate::diag!("[share] send: {} in {}ms", summary(&out), started.elapsed().as_millis());
    to_c(out)
}

/// `ok sent=2 queued=1` or `failed network`: an outcome, never content.
fn summary(json: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    if v["ok"].as_bool() != Some(true) {
        return format!("failed {}", v["reason"].as_str().unwrap_or("internal"));
    }
    let count = |s: &str| {
        v["results"]
            .as_array()
            .map_or(0, |r| r.iter().filter(|x| x["status"] == s).count())
    };
    format!("ok sent={} queued={} rejected={}", count("sent"), count("queued"), count("rejected"))
}

/// The host app is leaving the foreground (`NSExtensionHostWillResignActive`): finish or park
/// the sends in flight within `timeout_ms`, then release `state.lock` and close the database.
/// True when nothing is held.
#[no_mangle]
pub extern "C" fn pollis_share_suspend(timeout_ms: u32) -> bool {
    std::panic::catch_unwind(|| session::suspend(Duration::from_millis(u64::from(timeout_ms))))
        .unwrap_or(false)
}

/// The host app is back (`NSExtensionHostDidBecomeActive`): reopen what
/// [`pollis_share_suspend`] closed.
#[no_mangle]
pub extern "C" fn pollis_share_resume() {
    let _ = std::panic::catch_unwind(session::resume);
}

/// End the session before completing or cancelling the extension request. Unfinished sends stay
/// in the outbox for the app.
#[no_mangle]
pub extern "C" fn pollis_share_close() {
    let _ = std::panic::catch_unwind(session::close);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_data_dir_is_internal_not_a_crash() {
        // SAFETY: NULL is allowed; the result is freed once.
        unsafe {
            let out = pollis_share_open(std::ptr::null());
            assert_eq!(CStr::from_ptr(out).to_str().unwrap(), INTERNAL);
            crate::notify::ffi::pollis_string_free(out);
        }
    }

    #[test]
    fn a_malformed_request_is_refused_without_a_session() {
        // SAFETY: valid strings; NULL progress is allowed.
        unsafe {
            let bad = CString::new("{not json").unwrap();
            let out = pollis_share_send(bad.as_ptr(), None, std::ptr::null_mut());
            assert_eq!(CStr::from_ptr(out).to_str().unwrap(), r#"{"ok":false,"reason":"invalid"}"#);
            crate::notify::ffi::pollis_string_free(out);
        }
    }

    #[test]
    fn targets_without_a_session_say_locked() {
        // SAFETY: NULL query is allowed.
        unsafe {
            session::close();
            let out = pollis_share_targets(std::ptr::null(), 0);
            let v: serde_json::Value = serde_json::from_str(CStr::from_ptr(out).to_str().unwrap()).unwrap();
            assert_eq!(v["ok"], false);
            assert_eq!(v["reason"], "locked");
            crate::notify::ffi::pollis_string_free(out);
        }
    }

    #[test]
    fn a_summary_counts_outcomes_never_content() {
        let ok = r#"{"ok":true,"results":[{"status":"sent"},{"status":"queued"},{"status":"sent"}]}"#;
        assert_eq!(summary(ok), "ok sent=2 queued=1 rejected=0");
        assert_eq!(summary(r#"{"ok":false,"reason":"network"}"#), "failed network");
    }
}
