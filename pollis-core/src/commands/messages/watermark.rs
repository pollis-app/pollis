//! The delivery-watermark computation — extracted as a pure, generic free
//! function so it can be (1) exercised by the real ingest path and (2) proved
//! by Kani over symbolic inputs.
//!
//! The watermark is an exclusive fetch cursor, so it must stop strictly below the first un-handled envelope (F3).
//! Kani proves this (P1), monotonicity (P2) and handled-liveness (P3), each paired with a mutant.

/// The only distinction the watermark cares about: whether an envelope's
/// deliverability is gated on reaching its MLS epoch this pass.
///
/// `Delete` is a redaction that took effect; `DeletePending` one that still can, held back like an unreached message (#693).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnvKind {
    Message,
    Edit,
    Delete,
    DeletePending,
    Other,
}

impl EnvKind {
    /// The string mapping only; ingest re-classifies an unapplied tombstone to `DeletePending`.
    pub fn from_type(env_type: &str) -> Self {
        match env_type {
            "message" => EnvKind::Message,
            "edit" => EnvKind::Edit,
            "delete" => EnvKind::Delete,
            _ => EnvKind::Other,
        }
    }
}

/// Is this envelope definitively handled, or must a later pass retry it? `max_fired_epoch` is how far replay reached.
pub(crate) fn is_handled<E: Ord + Copy>(
    kind: EnvKind,
    epoch: Option<E>,
    max_fired_epoch: Option<E>,
) -> bool {
    match kind {
        EnvKind::Message | EnvKind::Edit => match (epoch, max_fired_epoch) {
            // Within reach: decrypted now, or a pre-join epoch never decryptable — handled.
            (Some(e), Some(max)) => e <= max,
            // Unparseable bytes are never MLS-decryptable → permanently handled
            // (advancing past avoids wedging on a corrupt row).
            (None, _) => true,
            // The replay reached no epoch (no local group): nothing could be
            // decrypted, so these must be retried once a group exists.
            (Some(_), None) => false,
        },
        // An applied tombstone / unknown type is epoch-independent and handled.
        EnvKind::Delete | EnvKind::Other => true,
        // An unapplied tombstone is held back like an unreached message (#693).
        EnvKind::DeletePending => false,
    }
}

/// One envelope as the watermark rule sees it: sort key, kind, and `(generation, epoch)`.
pub type EnvView<S, E> = (S, EnvKind, Option<E>);

/// The greatest key in the handled prefix strictly below the first un-handled envelope, or `None`.
///
/// Keep in sync with `NextWatermark` in `specs/tla/Delivery.tla`.
pub fn next_watermark<S: Ord + Clone, E: Ord + Copy>(
    envs: &[EnvView<S, E>],
    max_fired_epoch: Option<E>,
) -> Option<S> {
    // The cursor of the first envelope we must retry is an EXCLUSIVE ceiling on
    // the watermark: advancing to (or, via a tie, past) it would drop it from the
    // next `cursor > watermark` fetch.
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
    //! Pins the tombstone asymmetry (#693 / #661 WS1): an unapplied redaction is `DeletePending` and retried.

    use super::{next_watermark, EnvKind, EnvView};

    /// An applied tombstone is consumed on the first pass regardless of group state.
    #[test]
    fn a_delete_tombstone_is_consumed_unconditionally() {
        // No group state reached this pass (`max_fired_epoch = None`).
        let envs: [EnvView<&str, (i64, u64)>; 1] = [("t1", EnvKind::Delete, None)];
        assert_eq!(
            next_watermark(&envs, None),
            Some("t1"),
            "an APPLIED delete tombstone advances the watermark past itself — its \
             redaction landed, so there is nothing to re-fetch"
        );
    }

    /// The fix (#693): an unapplied tombstone is `DeletePending` and not consumed.
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

    /// A `DeletePending` above an applied `Delete` holds the cursor below itself.
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

    /// A message at an unreached epoch is not consumed.
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

    /// #661: a tombstone above an undecryptable message is re-fetched, never stranded by the cursor.
    #[test]
    fn a_tombstone_above_an_unreached_message_does_not_advance_past_the_message() {
        let envs: [EnvView<&str, (i64, u64)>; 2] = [
            ("t1", EnvKind::Message, Some((0, 5))),
            ("t2", EnvKind::Delete, None),
        ];
        // The message (t1) is unhandled, so the cursor cannot reach t2 either.
        assert_eq!(
            next_watermark(&envs, None),
            None,
            "with an unreached message below it, the delete is not consumed this \
             pass — so a lost delete is an application failure, not a cursor bug"
        );
    }
}

// Kani proof harnesses (`#[cfg(kani)]` only), bounded to `envs.len() <= 4`.
#[cfg(kani)]
mod proofs {
    use super::*;

    // Keys and epochs in `0..=3`; CBMC OOMs small boxes, so these run in CI (`kani.yml`).
    const MAX_LEN: usize = 4;

    impl kani::Arbitrary for EnvKind {
        fn any() -> Self {
            // Cover all five kinds, including `DeletePending`.
            match kani::any::<u8>() % 5 {
                0 => EnvKind::Message,
                1 => EnvKind::Edit,
                2 => EnvKind::Delete,
                3 => EnvKind::DeletePending,
                _ => EnvKind::Other,
            }
        }
    }

    /// A symbolic ascending envelope table in a stack array (CBMC chokes on `Vec`); keep MAX_LEN within the key domain.
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
            // Epoch only meaningful for message/edit; keep it bounded and present
            // only where the real parser would produce one.
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

    /// P1 (no-skip): the watermark is strictly below the first un-handled envelope's key.
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
            // Whether or not the watermark advanced, it must sit strictly below
            // the first envelope we still owe a retry.
            if let Some(w) = wm {
                assert!(w < stop);
            }
        }
    }

    /// P2 (monotone): a prefix's watermark never exceeds the full slice's; distinct keys only.
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

        // Option ordering: None < Some(_), so a prefix that produced no cursor
        // never exceeds the full slice's cursor.
        assert!(wm_prefix <= wm_full);
    }

    /// P3 (handled-liveness): all handled ⇒ watermark is the max key.
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
            // Strictly-increasing keys ⇒ the last element carries the max sent_at.
            Some((max_key, _, _)) => assert!(wm == Some(*max_key)),
            None => assert!(wm.is_none()),
        }
    }

    // Mutant: breaks on `>` instead of `>=`, so a tie skips an un-handled envelope (F3).
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
                // BUG: `>` lets a tie through where the real code uses `>=`.
                if sent_at > stop {
                    break;
                }
            }
            candidate = Some(sent_at.clone());
        }
        candidate
    }

    /// `should_panic`: passes exactly when Kani refutes P1 on the mutant.
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

    // P2 mutant: bails to `None` at the first un-handled envelope; only P2 catches it.
    fn next_watermark_p2_mutant<S: Ord + Clone, E: Ord + Copy>(
        envs: &[EnvView<S, E>],
        max_fired_epoch: Option<E>,
    ) -> Option<S> {
        let mut candidate: Option<S> = None;
        for (sent_at, kind, epoch) in envs {
            // BUG: abandon the good handled prefix instead of returning it.
            if !is_handled(*kind, *epoch, max_fired_epoch) {
                return None;
            }
            candidate = Some(sent_at.clone());
        }
        candidate
    }

    /// `should_panic`: passes exactly when Kani refutes P2 on the mutant.
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

    // P3 mutant: never adopts the final envelope; only P3 catches it.
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
            // BUG: never adopt the last envelope, even when handled.
            if i + 1 == n {
                break;
            }
            candidate = Some(sent_at.clone());
        }
        candidate
    }

    /// `should_panic`: passes exactly when Kani refutes P3 on the mutant.
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
