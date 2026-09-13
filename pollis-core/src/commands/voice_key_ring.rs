//! Voice E2EE key-ring slot arithmetic, in one always-compiled place.
//!
//! libwebrtc's `FrameCryptor` keeps a small per-participant **key ring**
//! (`KeyProviderOptions::key_ring_size`, 16). A sender encrypts every frame
//! with the key at its own `key_index` and writes that index into the frame
//! trailer; a receiver reads the index off the trailer and decrypts with
//! whatever key it holds in that slot. `KeyProvider::set_shared_key(key, idx)`
//! refuses (`false`) any `idx >= key_ring_size`, and the cryptor never moves
//! off the slot it was created on unless someone calls
//! `FrameCryptor::set_key_index`.
//!
//! Pollis maps MLS epochs onto that ring: the key for epoch `e` lives in slot
//! `e % ring_size`. This module is the single source of truth for that mapping
//! so the join path, the rotation hook and the old-slot scrub can never
//! disagree about which slot an epoch occupies — and so the mapping is
//! unit-tested on every target, including headless builds where LiveKit does
//! not link (the same split `livekit_identity` uses).

/// Slots in the libwebrtc key ring. Passed explicitly to
/// `KeyProviderOptions` by `voice_e2ee::build_e2ee_options` so the ring the
/// cryptor allocates and the modulus this module reduces by are the same
/// constant, not two defaults that happen to coincide. 16 is also what
/// `livekit-client` JS uses, so cross-SDK peers stay interoperable.
pub const VOICE_KEY_RING_SIZE: i32 = 16;

/// Grace period after a rotation during which the *previous* epoch's slot
/// stays populated so in-flight frames — and frames from a peer that has not
/// processed the commit yet — still decrypt. After it the slot is scrubbed
/// (overwritten with random bytes) so an ex-member who still holds the old
/// epoch's key cannot keep injecting frames under the old index.
pub const VOICE_KEY_SLOT_GRACE_SECS: u64 = 10;

/// The key-ring slot the voice key for `epoch` occupies. Always in
/// `0..VOICE_KEY_RING_SIZE`, so a `set_shared_key` at this index can never be
/// rejected by the ring bound.
pub fn voice_key_index(epoch: u64) -> i32 {
    (epoch % VOICE_KEY_RING_SIZE as u64) as i32
}

/// Whether `slot` was re-populated by some epoch in `(after_epoch, current]`.
/// Used by the post-rotation scrub: if a newer epoch has since landed in the
/// slot we were about to wipe, wiping it would destroy a live key.
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
        // The old derivation, `(epoch & 0x7FFF_FFFF) as i32`, produced 16 for
        // epoch 16 — an index libwebrtc rejects, so the rotation silently
        // installed nothing. Every epoch must now land inside the ring.
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
