//! The plain C ABI (#1599). Declared in `pollis-core/include/pollis_notify.h`.
//!
//! Every function here is callable from any thread, before or without `init_pollis`, and never
//! returns NULL from a string-returning call.

use std::ffi::{c_char, CStr, CString};
use std::time::Duration;

/// Budget used when the host passes 0.
const DEFAULT_BUDGET_MS: u32 = 20_000;

const INTERNAL: &str = r#"{"ok":false,"reason":"internal"}"#;

/// Fetch, decrypt and preview pending messages. Returns an owned JSON string; free it with
/// [`pollis_string_free`].
///
/// # Safety
/// `data_dir` must be NULL or a valid NUL-terminated string; so must `handle`.
#[no_mangle]
pub unsafe extern "C" fn pollis_notify_fetch(
    data_dir: *const c_char,
    handle: *const c_char,
    budget_ms: u32,
) -> *mut c_char {
    let data_dir = opt_str(data_dir);
    let handle = opt_str(handle);
    let json = std::panic::catch_unwind(move || fetch_json(data_dir, handle, budget_ms))
        .unwrap_or_else(|_| INTERNAL.to_string());
    to_c(json)
}

/// Free a string returned by this library. NULL is a no-op.
///
/// # Safety
/// `s` must come from this library and not have been freed already.
#[no_mangle]
pub unsafe extern "C" fn pollis_string_free(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    drop(CString::from_raw(s));
}

/// Relocate the data dir (iOS: the App Group container) without changing the keystore
/// namespace. The app calls it before `init_pollis`; `init_pollis` then migrates once.
///
/// # Safety
/// `dir` must be NULL (ignored) or a valid NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn pollis_set_data_dir_override(dir: *const c_char) {
    if let Some(dir) = opt_str(dir).filter(|d| !d.is_empty()) {
        crate::db::local::set_data_dir_override(dir);
    }
}

/// iOS: the keychain access group shared with the extension, e.g.
/// `"<TEAMID>.com.pollis.shared"`. Overrides the `PollisKeychainAccessGroup` Info.plist key.
/// A no-op elsewhere.
///
/// # Safety
/// `group` must be NULL (ignored) or a valid NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn pollis_set_keychain_access_group(group: *const c_char) {
    #[cfg(target_os = "ios")]
    if let Some(group) = opt_str(group) {
        crate::keystore::set_ios_keychain_access_group(&group);
    }
    #[cfg(not(target_os = "ios"))]
    let _ = group;
}

/// The app is entering the background: call SYNCHRONOUSLY in `applicationDidEnterBackground`,
/// before dispatching anything, and pass the result to [`pollis_app_will_suspend`].
#[no_mangle]
pub extern "C" fn pollis_app_suspend_ticket() -> u64 {
    super::lock::suspend::ticket()
}

/// Off the main thread, inside a `beginBackgroundTask` assertion (it blocks up to
/// `timeout_ms`): stop new holds of `state.lock`, wait for the open ones, then checkpoint and
/// close the app's database connection, so the suspended app holds no lock in the App Group
/// container (`0xdead10cc`). Does nothing if [`pollis_app_did_resume`] ran since `ticket`.
/// Returns true when nothing is held.
#[no_mangle]
pub extern "C" fn pollis_app_will_suspend(ticket: u64, timeout_ms: u32) -> bool {
    std::panic::catch_unwind(|| {
        let drained =
            super::lock::suspend::begin(ticket, Duration::from_millis(u64::from(timeout_ms)));
        let closed = crate::bridge::quiesce_for_suspend(ticket);
        drained && closed
    })
    .unwrap_or(false)
}

/// The app is back in the foreground: holds may start again and the database reopens.
/// Blocks briefly (the reopen); safe on any thread.
#[no_mangle]
pub extern "C" fn pollis_app_did_resume() {
    let _ = std::panic::catch_unwind(crate::bridge::resume_after_suspend);
}

fn fetch_json(data_dir: Option<String>, handle: Option<String>, budget_ms: u32) -> String {
    let Some(data_dir) = data_dir.filter(|d| !d.is_empty()) else {
        return INTERNAL.to_string();
    };
    let data_dir = std::path::PathBuf::from(data_dir);
    let budget = Duration::from_millis(u64::from(if budget_ms == 0 {
        DEFAULT_BUDGET_MS
    } else {
        budget_ms
    }));

    super::host::adopt_data_dir(&data_dir);
    // An extension never mints a keystore key; see `keystore::forbid_key_creation`. Only when
    // the app is not running in this process (Android may call this in-process).
    if !crate::bridge::is_initialized() {
        crate::keystore::forbid_key_creation();
    }
    let Some(config) = super::host::load_config(&data_dir) else {
        // The app has never started on this build: nothing is set up for a headless fetch.
        return super::fetch::Outcome::Failed(super::fetch::Reason::NotLoggedIn).to_json();
    };
    let params = super::fetch::Params {
        keystore: crate::keystore::default_os_keystore(),
        config,
        handle,
        budget,
        #[cfg(any(test, feature = "test-harness"))]
        local_dir: None,
    };
    // The bridge's big-stack workers: an extension thread's stack is far too small for
    // SQLCipher and MLS, and one shared runtime keeps the HTTP pool consistent in-process.
    let rt = crate::bridge::worker_runtime();
    match rt.block_on(rt.spawn(super::fetch::run(params))) {
        Ok(outcome) => outcome.to_json(),
        Err(_) => INTERNAL.to_string(),
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_data_dir_is_internal_not_a_crash() {
        // SAFETY: NULL is allowed for both; the result is freed once.
        unsafe {
            let out = pollis_notify_fetch(std::ptr::null(), std::ptr::null(), 10);
            assert!(!out.is_null());
            assert_eq!(CStr::from_ptr(out).to_str().unwrap(), INTERNAL);
            pollis_string_free(out);
            pollis_string_free(std::ptr::null_mut());
        }
    }
}
