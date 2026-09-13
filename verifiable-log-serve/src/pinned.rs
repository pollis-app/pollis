//! Pinned transparency-log public key(s) for the auditor CLI (H-1 fix).
//!
//! Before this, `verify_remote`/group/account/release fetched `public_key.json`
//! from the server and verified STH signatures against whatever key it
//! advertised — so a hostile or MITM'd log could serve its own key plus a
//! self-consistent forged log and every check would pass. The published
//! contract (SECURITY.md, docs/verify-transparency-log.md) is that the verifier
//! trusts ONLY the pinned key, never the served one. [`retain_pinned`] drops any
//! served candidate that is not pinned, comparing on the id RECOMPUTED from the
//! served key bytes (never the server-supplied id), so an equivocating log fails
//! closed. The compiled value mirrors
//! `pollis_core::commands::transparency::PINNED_LOG_PUBLIC_KEYS` (the client's
//! pin) — the two must stay identical.
use verifiable_log::{key_id_for, verifying_key_from_hex, VerifyingKey};

/// Pinned ML-DSA-44 log signing public key(s), lowercase hex (1312 bytes each).
/// A list so a rotation can publish the new key alongside the retiring one
/// during the overlap window.
pub const PINNED_LOG_PUBLIC_KEYS_HEX: &[&str] = &[
    "56ab128f3f10107382802e69d3de8659d0127c711feb9c849f5b213c6f2d0af3b5fe41f581b202b385906fc42e4421747e84939054d160c551536131e41508a82b1f3ff0a07bcc4cee5e2eae8e85155d5c9e0dbc6e7683811649fb9e3b1f18c7ed070dbf61f2a058915b33f8ad3edcd135dd18770053e5ac971b13d17d95e16e98f47a852d600c47cbc0349354af2898803cfec7112660076d20027cb67870e18fb25ee327a36743fa812ccf93ba0769ddbd3d42ab40849ac8c98357b64eaf1ffc242abb12fddef4d8cdfa02448b4d99546b448e589657f898a47c6f30ddd88edd3f4456470e0a151e5fd601750c8b0489d3471897cfa78e0d7a00d938dfe876ef243117c972e041fdb00aa7af30d34184153cfd7b1e3b481dc562bbfc82bc20fe8ac4d9845f41de49fc33b6f94494df7088b06c7cb9ae35db86ac0fd293ca403046cec46ca9b12c755670d3d9b14c300b11ec292cd5e37d9f9e5e5d1729222a33bf1e13440f44dbf1b4d4104c612db4e269760868be5ff99f9ed269625fa4f39e21713a14293285e95f8a8e8cecd9db8e6a70c36340280322eab3490270ac640f706a23e81d79111dead641eaf7b926582ed0b0422f9addc0091d731a4fe1b9079be8bd75df23f5f9bf287beab7f67f763e04f0245bf9c705136d04eb8391fb4b4f12bfba44ae49bb6f32ddb0d539e59cd0159120b2fb1718f57e12a846638dbe0b650bfcd5a6cc74cd315b49136ea4e13d431a7f3a4c38fc783a82ca2b4c44a2f379c8aa9704d4639de3f94466662c97fbbd834db97a90405c382b5039803f4e4ed5c6b57487c8d23ad9e4d319df3466c49ef1e1cef526ddad1db5fa14f3b067b40580e068582dc428e21dbdc3df848e8e00fe1181f8e0d1409ab9a8757aef008b67191f4368f37cbd587ff65acdf07adbb989d09cc3318e346ca71c029557f2c523c204defab472b3dcb09bfbb95d5d1665a360a00faeb09b660f13fdc00f7b53fbfeaa58f87a208ad4551bcbe4307bf4d8451e027f4cc33cd55700016795c3164b1bc90d9dd1737b49d2e9e4b190128d2e62a44a80c1375c616aa2871ae7ad4a914102551380a8f8edb68c2df02bdf52607a7432ea7026f6a1efcdb37ecc11ecf1623ec6979e5d65c2812a997121010cd5fd9a98b9ed34edc17b667bfd37ef2be6dfe67fbdde03fa95bb80d0e1c7336263042ef44c4f9d28f1bf959bdc24c09cf8269378705022ff476fce91dbba6c8ffec00b27572eaa4835b59948d7a625ccc84ff4ac062176f4972f5131a961b17c7ff0010d2f2f3f8c12b7bf05fb9771d64a24fdab058f4bf3a155ade6a496b9a09d43a7673b5d8fb6519e01bf911ca78cc23f95943f63db72883d522fe24d4b7c7a26c7fd43b4f6f7496acf9ea2cab2e3cd6fc274964b576084c820bae79dbaa331d11751ec718660cd8e7847b7bacf31180803f681fb349b96338c98c791f74bc95e0d37b2810632159bc3175fed2e16038d45d35e4628250e8c9fb66c5bb2238f6456901f657e9655d3d5a09ff4952a0b9eb9c614f78c27626a136ef281f7099f68e898628530ef690851c179ef6a02448d498e49b2c362c839832100f4a9bf4abf17d496c71bfb5263da345d952b275f04707b31b9f6575da6dd2be799b90cc615f52ec32b4833a7e619d7f34f91f16edc38bc0a869c7211473f3ab90255446e0b7efbb2b97e8111d43b039ec0469b020f38925aad61e229836c96fad5bf3c3cad8f2c1c8b56cd819e8972d108dbfa8cd518177feaa7f4e0b547584a9a5d39ad4f1e8010cfead998ec18991cb89031a11c03cbd1ee7e0a1436da10ef154db13d4850c687c0a668215c9c8b7b1c",
];

/// Env var (all builds): extra pinned keys, comma/whitespace-separated hex, for
/// auditing a non-production log you run yourself (the docs' "`<base-url>` is a
/// dev server you run yourself" case). Additive to the compiled pin and set by
/// the local operator only — the untrusted party here is the SERVER, which
/// cannot set your env, so this does not reopen the finding.
const PIN_ENV: &str = "POLLIS_VERIFY_PINNED_KEYS_HEX";

/// In-process allowance, populated by a server in this crate when it starts
/// serving a log: [`crate::DevServer::spawn`] and [`crate::LiveServer::spawn`].
///
/// Safe to keep in every profile, because only a process that is ITSELF serving
/// a log can add to it. `pollis-verify` — the auditor CLI, the thing the pin
/// exists to protect — constructs neither server, so nothing can widen its
/// trust. What this does enable is the `serve` binary's own dynamic
/// `/verify/group/<id>` endpoint, and this crate's tests, verifying a log signed
/// by a dev key rather than the production one.
///
/// It was `#[cfg(debug_assertions)]` at first, which made the crate's tests pass
/// or fail on the build profile AND on scheduling — a test that verified without
/// spawning a server only worked if some other test in the binary happened to
/// spawn one first. The real fix was narrowing the pin to the fetch boundary
/// (see [`require_pinned`]); this is no longer load-bearing for the in-bundle
/// paths, and no longer profile-dependent.
static DEV_EXTRA: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Trust `hex` in addition to the compiled pin, for this process only. Called by
/// a server in this crate with the key of the log it is about to serve.
pub fn trust_hex_for_dev(hex: &str) {
    if let Ok(mut v) = DEV_EXTRA.lock() {
        if !v.iter().any(|h| h == hex) {
            v.push(hex.to_string());
        }
    }
}

fn parse_hex_list(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split([',', ' ', '\t', '\n', '\r'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// The pinned keys parsed to `(key_id, VerifyingKey)`: the compiled pin, plus the
/// env override, plus (debug only) the dev-server allowance. A malformed entry
/// drops OUT of the set (never silently widens trust).
pub fn pinned_candidates() -> Vec<(String, VerifyingKey)> {
    let mut hexes: Vec<String> = PINNED_LOG_PUBLIC_KEYS_HEX
        .iter()
        .map(|s| s.to_string())
        .collect();
    if let Ok(env) = std::env::var(PIN_ENV) {
        hexes.extend(parse_hex_list(&env));
    }
    if let Ok(v) = DEV_EXTRA.lock() {
        hexes.extend(v.iter().cloned());
    }
    hexes
        .iter()
        .filter_map(|hex| verifying_key_from_hex(hex).ok().map(|vk| (key_id_for(&vk), vk)))
        .collect()
}

/// Keep only the served candidates whose key is pinned. Records one report check
/// ("served public_key.json is the pinned log key"): it passes iff at least one
/// served key matches the pin. Comparison is keyed off the id RECOMPUTED from the
/// served key bytes, never the id the server supplied alongside them — otherwise
/// an attacker serves its own key labelled with the pinned key's id and slips
/// through. Returns the pinned-and-served subset; callers must treat an empty
/// result as "refuse to trust this log".
pub fn retain_pinned<F: FnMut(bool, String)>(
    served: Vec<(String, VerifyingKey)>,
    check: &mut F,
) -> Vec<(String, VerifyingKey)> {
    let pinned = pinned_candidates();
    let pinned_ids: std::collections::HashSet<String> =
        pinned.iter().map(|(_, vk)| key_id_for(vk)).collect();
    let kept: Vec<(String, VerifyingKey)> = served
        .into_iter()
        .filter(|(_, vk)| pinned_ids.contains(&key_id_for(vk)))
        .collect();
    check(
        !kept.is_empty(),
        "served public_key.json is the pinned log key".to_string(),
    );
    kept
}

/// Refuse a SERVED key document whose key is not pinned.
///
/// This is the fetch boundary: `verify_*_via` pulls `public_key.json` off an
/// untrusted server, builds a [`crate::bundle::Bundle`] around it, and hands
/// that to the `*_in_bundle` core. The pin has to be applied HERE, on the way
/// in — not inside the core, which is also used to verify a bundle the caller
/// already holds (the publisher precomputing its own per-release report). A
/// bundle you built yourself is correctly verified against its own key; there is
/// no third party in that path to distrust.
pub fn require_pinned(doc: &crate::bundle::PublicKeyDoc, now_ms: u64) -> Result<(), String> {
    let mut passed = false;
    let _ = retain_pinned(doc.verifying_candidates(now_ms), &mut |ok, _| passed = ok);
    if passed {
        Ok(())
    } else {
        Err("served public_key.json does not match the pinned log key — refusing to trust \
             the served log"
            .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // brings `verifying_key()` onto the signing key
    use ml_dsa::Keypair as _;

    fn pinned_vk() -> VerifyingKey {
        verifying_key_from_hex(PINNED_LOG_PUBLIC_KEYS_HEX[0]).expect("pinned key parses")
    }
    // A valid ML-DSA-44 key that is NOT pinned (stands in for an attacker key).
    fn foreign_vk() -> VerifyingKey {
        verifiable_log::SigningKey::from_seed(&[3u8; 32].into()).verifying_key()
    }
    fn run(served: Vec<(String, VerifyingKey)>) -> (Vec<(String, VerifyingKey)>, bool) {
        let mut passed = None;
        let kept = retain_pinned(served, &mut |ok, _| passed = Some(ok));
        (kept, passed.expect("retain_pinned records exactly one check"))
    }

    #[test]
    fn the_pinned_key_parses_and_is_present() {
        assert_eq!(pinned_candidates().len(), PINNED_LOG_PUBLIC_KEYS_HEX.len());
    }

    #[test]
    fn keeps_the_pinned_key_even_under_a_forged_server_supplied_id() {
        // The server labels the (genuine) pinned key with a bogus id; we must
        // still keep it because the match is on the recomputed key bytes.
        let (kept, ok) = run(vec![("bogus-server-id".to_string(), pinned_vk())]);
        assert_eq!(kept.len(), 1);
        assert!(ok, "the pinned key must verify regardless of its served id");
    }

    #[test]
    fn refuses_a_foreign_key_relabelled_with_the_pinned_id() {
        // H-1 regression: the exact equivocation the finding is about — serve a
        // DIFFERENT key under the pinned key's id. Keying off the served id would
        // wrongly accept it; keying off the recomputed key bytes rejects it.
        let pinned_id = key_id_for(&pinned_vk());
        let (kept, ok) = run(vec![(pinned_id, foreign_vk())]);
        assert!(kept.is_empty(), "a foreign key must be refused even with the pinned key's id");
        assert!(!ok, "the pin check must fail when no served key is genuinely pinned");
    }

    #[test]
    fn refuses_when_only_foreign_keys_are_served() {
        let (kept, ok) = run(vec![("k".to_string(), foreign_vk())]);
        assert!(kept.is_empty());
        assert!(!ok);
    }
}
