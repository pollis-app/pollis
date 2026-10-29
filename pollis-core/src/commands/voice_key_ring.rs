//! Voice E2EE key-ring slot arithmetic: the key for MLS epoch `e` lives in
//! libwebrtc key-ring slot `e % ring_size`. The single source of truth so the
//! join path, rotation hook and old-slot scrub agree; always compiled so it is
//! unit-tested on every target.
//!
//! `KeyProvider::set_shared_key` rejects any index `>= key_ring_size`.

/// Slots in the libwebrtc key ring, passed explicitly to `KeyProviderOptions` so
/// the ring and this modulus can't diverge. Matches `livekit-client` JS for
/// cross-SDK interop.
pub const VOICE_KEY_RING_SIZE: i32 = 16;

/// How long the previous epoch's slot stays populated after a rotation, so
/// in-flight frames (and peers yet to process the commit) still decrypt. Then it
/// is scrubbed so an ex-member can't inject frames under the old index.
pub const VOICE_KEY_SLOT_GRACE_SECS: u64 = 10;

/// The slot the voice key for `epoch` occupies; always inside the ring.
pub fn voice_key_index(epoch: u64) -> i32 {
    (epoch % VOICE_KEY_RING_SIZE as u64) as i32
}

/// Whether `slot` was re-populated by an epoch in `(after_epoch, current]`, in
/// which case scrubbing it would destroy a live key.
pub fn slot_reused_since(slot: i32, after_epoch: u64, current_epoch: u64) -> bool {
    if current_epoch <= after_epoch {
        return false;
    }
    // Once the ring has gone all the way round, every slot has been reused.
    if current_epoch - after_epoch >= VOICE_KEY_RING_SIZE as u64 {
        return true;
    }
    (after_epoch + 1..=current_epoch).any(|e| voice_key_index(e) == slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_always_fits_the_ring() {
        for epoch in [0u64, 1, 15, 16, 17, 31, 32, 1_000, u32::MAX as u64, u64::MAX] {
            let idx = voice_key_index(epoch);
            assert!(
                (0..VOICE_KEY_RING_SIZE).contains(&idx),
                "epoch {epoch} mapped to slot {idx}, outside 0..{VOICE_KEY_RING_SIZE}"
            );
        }
        assert_eq!(voice_key_index(16), 0);
        assert_eq!(voice_key_index(17), 1);
    }

    #[test]
    fn consecutive_epochs_never_share_a_slot() {
        for epoch in 0..64u64 {
            assert_ne!(voice_key_index(epoch), voice_key_index(epoch + 1));
        }
    }

    #[test]
    fn slots_collide_exactly_one_ring_apart() {
        // Known residual: two live epochs a full ring apart share a slot.
        assert_eq!(voice_key_index(3), voice_key_index(3 + VOICE_KEY_RING_SIZE as u64));
    }

    #[test]
    fn scrub_skips_a_slot_a_newer_epoch_reoccupied() {
        let old_slot = voice_key_index(4);
        assert!(!slot_reused_since(old_slot, 5, 5));
        assert!(!slot_reused_since(old_slot, 5, 19));
        assert!(slot_reused_since(old_slot, 5, 20));
        assert!(slot_reused_since(old_slot, 5, 500));
        // A rejoin that reset the epoch below the rotation point is not a reuse.
        assert!(!slot_reused_since(old_slot, 5, 2));
    }
}
