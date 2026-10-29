//! The delivery-watermark computation: a pure, generic function used by the
//! real ingest path and proved by Kani over symbolic inputs.
//!
//! The watermark is an exclusive cursor (the next fetch asks for
//! `cursor > watermark`), so it must never reach an envelope that still needs a
//! retry, or a current member permanently loses a decryptable message (failure
//! class F3). The cursor is the DS-assigned per-conversation sequence (#1087);
//! ties are therefore impossible, but the `>=` stop rule in [`next_watermark`]
//! stays correct even with ties.
//!
//! The Kani harnesses below prove no-skip (P1), monotonicity (P2) and
//! handled-liveness (P3), each paired with a mutant harness
//! (`p{1,2,3}_mutant_refuted`, `#[kani::should_panic]`) showing the proof has
//! teeth.

/// The only distinction the watermark cares about: whether an envelope's
/// deliverability is gated on reaching its MLS epoch this pass.
///
/// `Message` / `Edit` are epoch-gated. A delete tombstone is epoch-independent,
/// and its kind is the outcome of applying its redaction on this device (#693):
/// `Delete` (took effect, or never can) is handled; `DeletePending` (can still
/// take effect later) is held back like an unreached message. `Other` is always
/// handled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnvKind {
    Message,
    Edit,
    Delete,
    DeletePending,
    Other,
}

impl EnvKind {
    /// Map a `message_envelope.type` string to its kind. `"delete"` maps to
    /// `Delete`; ingest re-classifies one whose redaction did not take effect
    /// to `DeletePending` before calling [`next_watermark`].
    pub fn from_type(env_type: &str) -> Self {
        match env_type {
            "message" => EnvKind::Message,
            "edit" => EnvKind::Edit,
            "delete" => EnvKind::Delete,
            _ => EnvKind::Other,
        }
    }
}

/// Whether the watermark may advance over this envelope, or a later pass must
/// retry it. `max_fired_epoch` is the furthest position the replay reached this
/// pass (`None`: no local group). Also asked by ingest about a tombstone's
/// target.
pub(crate) fn is_handled<E: Ord + Copy>(
    kind: EnvKind,
    epoch: Option<E>,
    max_fired_epoch: Option<E>,
) -> bool {
    match kind {
        EnvKind::Message | EnvKind::Edit => match (epoch, max_fired_epoch) {
            // Within this pass's reach: decrypted now, or an unreachable
            // pre-join epoch. Either way handled.
            (Some(e), Some(max)) => e <= max,
            // Unparseable bytes are never decryptable; advancing avoids wedging.
            (None, _) => true,
            // No local group: retry once one exists.
            (Some(_), None) => false,
        },
        EnvKind::Delete | EnvKind::Other => true,
        // An unapplied tombstone holds the cursor (#693).
        EnvKind::DeletePending => false,
    }
}

/// One envelope as the watermark rule sees it: sort key, kind, and the
/// `(generation, epoch)` position a message needs before it can be decrypted.
pub type EnvView<S, E> = (S, EnvKind, Option<E>);

/// The key a conversation's watermark may advance to: the greatest key of the
/// contiguous handled prefix, strictly below the first un-handled envelope's
/// key. `None` if nothing may advance.
///
/// Generic over the key `S` so callers pass real cursors while the proofs pass
/// bounded integers (Kani cannot make a `String` symbolic). Generic over the
/// epoch key `E` because the real position is a lexicographic
/// `(generation, epoch)` pair (#454); the logic only uses `<=`, so proving over
/// a small `u64` domain loses no coverage.
///
/// Abstracted by `NextWatermark` in `specs/tla/Delivery.tla`; keep the "stop
/// strictly below the first un-handled envelope" rule in sync with it.
pub fn next_watermark<S: Ord + Clone, E: Ord + Copy>(
    envs: &[EnvView<S, E>],
    max_fired_epoch: Option<E>,
) -> Option<S> {
    // An exclusive ceiling: `>=` so a tie cannot carry the cursor onto it.
    let stop_at: Option<&S> = envs
        .iter()
        .find(|(_, kind, epoch)| !is_handled(*kind, *epoch, max_fired_epoch))
        .map(|(sent_at, _, _)| sent_at);

    let mut candidate: Option<S> = None;
    for (sent_at, _, _) in envs {
        if let Some(stop) = stop_at {
            if sent_at >= stop {
                break;
            }
        }
        candidate = Some(sent_at.clone());
    }
    candidate
}

#[cfg(test)]
mod delete_consumption_tests {
    //! #693: an applied tombstone is consumed; one whose redaction did not take
    //! effect (`DeletePending`) is retried like an unreached message. Which kind
    //! a tombstone gets is `ingest::delete_resolution_tests`.

    use super::{next_watermark, EnvKind, EnvView};

    /// An applied tombstone is consumed whatever the group state.
    #[test]
    fn a_delete_tombstone_is_consumed_unconditionally() {
        let envs: [EnvView<&str, (i64, u64)>; 1] = [("t1", EnvKind::Delete, None)];
        assert_eq!(
            next_watermark(&envs, None),
            Some("t1"),
            "an APPLIED delete tombstone advances the watermark past itself — its \
             redaction landed, so there is nothing to re-fetch"
        );
    }

    /// A tombstone whose redaction did not take effect is not consumed.
    #[test]
    fn a_pending_delete_is_retried_not_consumed() {
        let envs: [EnvView<&str, (i64, u64)>; 1] = [("t1", EnvKind::DeletePending, None)];
        assert_eq!(
            next_watermark(&envs, None),
            None,
            "a tombstone whose redaction did not take effect must NOT be consumed — \
             it is re-fetched until the redaction lands (like an unreached message)"
        );
    }

    /// A pending tombstone above an applied one holds the cursor below itself.
    #[test]
    fn a_pending_delete_holds_the_cursor_below_itself() {
        let envs: [EnvView<&str, (i64, u64)>; 2] = [
            ("t1", EnvKind::Delete, None),
            ("t2", EnvKind::DeletePending, None),
        ];
        assert_eq!(
            next_watermark(&envs, None),
            Some("t1"),
            "the cursor advances over the applied tombstone (t1) but stops strictly \
             below the pending one (t2), which is re-fetched next pass"
        );
    }

    /// A message at an unreached epoch is retried, not consumed.
    #[test]
    fn a_message_at_an_unreached_epoch_is_retried_not_consumed() {
        let envs: [EnvView<&str, (i64, u64)>; 1] =
            [("t1", EnvKind::Message, Some((0, 5)))];
        assert_eq!(
            next_watermark(&envs, None),
            None,
            "an undecryptable message must NOT be consumed — it is re-fetched until \
             its epoch is reached (unlike a delete tombstone)"
        );
    }

    /// A tombstone above an unreached message cannot move the cursor past the
    /// message (#661): a lost delete is an application failure, not a cursor bug.
    #[test]
    fn a_tombstone_above_an_unreached_message_does_not_advance_past_the_message() {
        let envs: [EnvView<&str, (i64, u64)>; 2] = [
            ("t1", EnvKind::Message, Some((0, 5))),
            ("t2", EnvKind::Delete, None),
        ];
        assert_eq!(
            next_watermark(&envs, None),
            None,
            "with an unreached message below it, the delete is not consumed this \
             pass — so a lost delete is an application failure, not a cursor bug"
        );
    }
}

// Kani proof harnesses, `#[cfg(kani)]` only. Bounded to `envs.len() <= 4`;
// keys/epochs/kinds are symbolic over a small domain, slices sorted ascending
// like the real caller's.
#[cfg(kani)]
mod proofs {
    use super::*;

    // Keys and epochs in `0..=3`. The no-skip / tie counterexamples are 2-3
    // element phenomena, so len 4 finds them. CBMC needs more memory than a
    // dev box has; these run in CI (`kani.yml`, 16 GB runner).
    const MAX_LEN: usize = 4;

    impl kani::Arbitrary for EnvKind {
        fn any() -> Self {
            // All five kinds, so the proofs cover `DeletePending` too.
            match kani::any::<u8>() % 5 {
                0 => EnvKind::Message,
                1 => EnvKind::Edit,
                2 => EnvKind::Delete,
                3 => EnvKind::DeletePending,
                _ => EnvKind::Other,
            }
        }
    }

    /// A symbolic, key-ascending envelope table in a fixed-size stack array plus
    /// a symbolic length. An array, not a `Vec`: CBMC's heap modelling OOMs even
    /// at small bounds. `distinct_keys` forces strictly increasing keys (P2/P3);
    /// with keys in `0..=3` and `MAX_LEN == 4` that is exactly `[0,1,2,3]`, so do
    /// not raise `MAX_LEN` past the key domain.
    fn symbolic_envs(distinct_keys: bool) -> ([EnvView<u8, u64>; MAX_LEN], usize) {
        let len: usize = kani::any();
        kani::assume(len <= MAX_LEN);

        let mut arr = [(0u8, EnvKind::Message, None); MAX_LEN];
        let mut prev: Option<u8> = None;
        for slot in arr.iter_mut() {
            let key: u8 = kani::any();
            kani::assume(key <= 3);
            if let Some(p) = prev {
                if distinct_keys {
                    kani::assume(key > p);
                } else {
                    kani::assume(key >= p);
                }
            }
            prev = Some(key);

            let kind: EnvKind = kani::any();
            let epoch: Option<u64> = if kani::any() {
                let e: u64 = kani::any();
                kani::assume(e <= 3);
                Some(e)
            } else {
                None
            };
            *slot = (key, kind, epoch);
        }
        (arr, len)
    }

    fn symbolic_max_fired() -> Option<u64> {
        if kani::any() {
            let m: u64 = kani::any();
            kani::assume(m <= 3);
            Some(m)
        } else {
            None
        }
    }

    /// P1 (no-skip): the watermark is strictly below the first un-handled
    /// envelope's key.
    #[kani::proof]
    #[kani::unwind(5)]
    fn p1_no_skip() {
        let (arr, len) = symbolic_envs(false);
        let envs = &arr[..len];
        let max_fired = symbolic_max_fired();

        let first_unhandled = envs
            .iter()
            .find(|(_, kind, epoch)| !is_handled(*kind, *epoch, max_fired))
            .map(|(k, _, _)| *k);

        let wm = next_watermark(envs, max_fired);

        if let Some(stop) = first_unhandled {
            if let Some(w) = wm {
                assert!(w < stop);
            }
        }
    }

    /// P2 (monotone): a prefix's watermark never exceeds the full slice's.
    /// Distinct keys only: under a tie the cursor is correctly pulled back.
    #[kani::proof]
    #[kani::unwind(5)]
    fn p2_monotone() {
        let (arr, len) = symbolic_envs(true);
        let envs = &arr[..len];
        let max_fired = symbolic_max_fired();

        let cut: usize = kani::any();
        kani::assume(cut <= envs.len());
        let prefix = &envs[..cut];

        let wm_prefix = next_watermark(prefix, max_fired);
        let wm_full = next_watermark(envs, max_fired);

        assert!(wm_prefix <= wm_full);
    }

    /// P3 (handled-liveness): if every envelope is handled, the watermark is the
    /// max key.
    #[kani::proof]
    #[kani::unwind(5)]
    fn p3_handled_liveness() {
        let (arr, len) = symbolic_envs(true);
        let envs = &arr[..len];
        let max_fired = symbolic_max_fired();

        let all_handled = envs
            .iter()
            .all(|(_, kind, epoch)| is_handled(*kind, *epoch, max_fired));
        kani::assume(all_handled);

        let wm = next_watermark(envs, max_fired);

        match envs.last() {
            Some((max_key, _, _)) => assert!(wm == Some(*max_key)),
            None => assert!(wm.is_none()),
        }
    }

    // P1 mutant: breaks on `>` instead of `>=`, so a tie lets the cursor onto an
    // un-handled envelope (the F3 bug).
    fn next_watermark_mutant<S: Ord + Clone, E: Ord + Copy>(
        envs: &[EnvView<S, E>],
        max_fired_epoch: Option<E>,
    ) -> Option<S> {
        let stop_at: Option<&S> = envs
            .iter()
            .find(|(_, kind, epoch)| !is_handled(*kind, *epoch, max_fired_epoch))
            .map(|(sent_at, _, _)| sent_at);

        let mut candidate: Option<S> = None;
        for (sent_at, _, _) in envs {
            if let Some(stop) = stop_at {
                // BUG: `>` where the real code uses `>=`.
                if sent_at > stop {
                    break;
                }
            }
            candidate = Some(sent_at.clone());
        }
        candidate
    }

    /// P1 on the mutant; passes only if Kani finds the counterexample, so a
    /// toothless proof fails this harness.
    #[kani::proof]
    #[kani::should_panic]
    #[kani::unwind(5)]
    fn p1_mutant_refuted() {
        let (arr, len) = symbolic_envs(false);
        let envs = &arr[..len];
        let max_fired = symbolic_max_fired();

        let first_unhandled = envs
            .iter()
            .find(|(_, kind, epoch)| !is_handled(*kind, *epoch, max_fired))
            .map(|(k, _, _)| *k);

        let wm = next_watermark_mutant(envs, max_fired);

        if let Some(stop) = first_unhandled {
            if let Some(w) = wm {
                assert!(w < stop);
            }
        }
    }

    // P2 mutant: returns `None` at the first un-handled envelope instead of the
    // handled run below it. Satisfies P1 and P3; only P2 catches it.
    fn next_watermark_p2_mutant<S: Ord + Clone, E: Ord + Copy>(
        envs: &[EnvView<S, E>],
        max_fired_epoch: Option<E>,
    ) -> Option<S> {
        let mut candidate: Option<S> = None;
        for (sent_at, kind, epoch) in envs {
            // BUG: abandon the handled prefix.
            if !is_handled(*kind, *epoch, max_fired_epoch) {
                return None;
            }
            candidate = Some(sent_at.clone());
        }
        candidate
    }

    /// P2 on the mutant; passes only if Kani finds a regression.
    #[kani::proof]
    #[kani::should_panic]
    #[kani::unwind(5)]
    fn p2_mutant_refuted() {
        let (arr, len) = symbolic_envs(true);
        let envs = &arr[..len];
        let max_fired = symbolic_max_fired();

        let cut: usize = kani::any();
        kani::assume(cut <= envs.len());
        let prefix = &envs[..cut];

        let wm_prefix = next_watermark_p2_mutant(prefix, max_fired);
        let wm_full = next_watermark_p2_mutant(envs, max_fired);

        assert!(wm_prefix <= wm_full);
    }

    // P3 mutant: never adopts the last envelope even when handled. Satisfies
    // P1; only P3 catches it.
    fn next_watermark_p3_mutant<S: Ord + Clone, E: Ord + Copy>(
        envs: &[EnvView<S, E>],
        max_fired_epoch: Option<E>,
    ) -> Option<S> {
        let stop_at: Option<&S> = envs
            .iter()
            .find(|(_, kind, epoch)| !is_handled(*kind, *epoch, max_fired_epoch))
            .map(|(sent_at, _, _)| sent_at);

        let n = envs.len();
        let mut candidate: Option<S> = None;
        for (i, (sent_at, _, _)) in envs.iter().enumerate() {
            if let Some(stop) = stop_at {
                if sent_at >= stop {
                    break;
                }
            }
            // BUG: never adopt the last envelope.
            if i + 1 == n {
                break;
            }
            candidate = Some(sent_at.clone());
        }
        candidate
    }

    /// P3 on the mutant; passes only if Kani finds a lagging watermark.
    #[kani::proof]
    #[kani::should_panic]
    #[kani::unwind(5)]
    fn p3_mutant_refuted() {
        let (arr, len) = symbolic_envs(true);
        let envs = &arr[..len];
        let max_fired = symbolic_max_fired();

        let all_handled = envs
            .iter()
            .all(|(_, kind, epoch)| is_handled(*kind, *epoch, max_fired));
        kani::assume(all_handled);

        let wm = next_watermark_p3_mutant(envs, max_fired);

        match envs.last() {
            Some((max_key, _, _)) => assert!(wm == Some(*max_key)),
            None => assert!(wm.is_none()),
        }
    }
}
