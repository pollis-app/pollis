//! Reads `HOME` through `wasi:cli/environment`; exists only to be refused.

#[no_mangle]
pub extern "C" fn probe() -> usize {
    std::env::var("HOME").map(|v| v.len()).unwrap_or(0)
}
