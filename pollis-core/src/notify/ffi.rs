//! The plain C ABI. See `pollis-core/include/pollis_notify.h`.

use std::ffi::{c_char, CString};

/// Fetch, decrypt and preview pending messages. Returns an owned JSON string;
/// free it with [`pollis_string_free`]. Never returns NULL, never unwinds.
///
/// # Safety
/// `data_dir` must be a valid NUL-terminated UTF-8 string; `handle` may be NULL.
#[no_mangle]
pub unsafe extern "C" fn pollis_notify_fetch(
    _data_dir: *const c_char,
    _handle: *const c_char,
    _budget_ms: u32,
) -> *mut c_char {
    to_c(r#"{"ok":false,"reason":"internal"}"#.to_string())
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

fn to_c(s: String) -> *mut c_char {
    CString::new(s)
        .unwrap_or_else(|_| CString::new(r#"{"ok":false,"reason":"internal"}"#).unwrap())
        .into_raw()
}
