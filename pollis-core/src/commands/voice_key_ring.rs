//! Voice E2EE key-ring slot arithmetic, in one always-compiled place.
//!
//! Epoch `e` → slot `e % 16`; the single mapping for join, rotation and scrub.

/// Key-ring slots, passed explicitly to `KeyProviderOptions`; matches JS (16).
pub const VOICE_KEY_RING_SIZE: i32 = 16;

/// Old-slot grace before it's scrubbed, so ex-members can't inject under it.
pub const VOICE_KEY_SLOT_GRACE_SECS: u64 = 10;

/// The ring slot for `epoch`; always in range.
pub fn voice_key_index(epoch: u64) -> i32 {
    (epoch % VOICE_KEY_RING_SIZE as u64) as i32
}

/// Whether `slot` was re-populated by some epoch in `(after_epoch, current]`.
/// If so, the scrub must skip it (it holds a live key).
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
        // The old `(epoch & 0x7FFF_FFFF)` gave 16 for epoch 16, which libwebrtc rejects.
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
        // A rotation installs epoch N+1 next to epoch N; if they collided the
        // new key would overwrite the one in-flight frames still need.
        for epoch in 0..64u64 {
            assert_ne!(voice_key_index(epoch), voice_key_index(epoch + 1));
        }
    }

    #[test]
    fn slots_collide_exactly_one_ring_apart() {
        // Documented residual: two live epochs a full ring apart share a slot.
        assert_eq!(voice_key_index(3), voice_key_index(3 + VOICE_KEY_RING_SIZE as u64));
    }

    #[test]
    fn scrub_skips_a_slot_a_newer_epoch_reoccupied() {
        // Rotated 4 → 5; the scrub of slot 4 must proceed while the group sits
        // at 5..19, and must be skipped once epoch 20 has re-populated it.
        let old_slot = voice_key_index(4);
        assert!(!slot_reused_since(old_slot, 5, 5));
        assert!(!slot_reused_since(old_slot, 5, 19));
        assert!(slot_reused_since(old_slot, 5, 20));
        assert!(slot_reused_since(old_slot, 5, 500));
        // A rejoin that reset the epoch below the rotation point is not a reuse
        // of this provider's slot.
        assert!(!slot_reused_since(old_slot, 5, 2));
    }
}
