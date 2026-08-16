//! Tiny crate-wide primitives that have no better home.
//!
//! Everything here was, until #875, copy-pasted into whichever module needed it
//! — `pin.rs`, `mls/ds_client.rs` and `net/overlay.rs` each carried their own
//! wall-clock helper under three different names. Identical copies are cheap
//! until one of them is edited.

/// Current wall-clock time, whole seconds since the Unix epoch.
///
/// A clock behind the epoch (only reachable if the system clock is wildly wrong)
/// reads as `0` rather than panicking: every caller uses this for TTLs and
/// rate-limit stamps, where "a very old timestamp" degrades gracefully and a
/// panic does not.
///
/// Returns `u64` because a Unix second is not negative. Callers whose downstream
/// arithmetic is signed — request-signature skew, which must not wrap when a
/// client's clock is ahead — cast at the boundary.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
