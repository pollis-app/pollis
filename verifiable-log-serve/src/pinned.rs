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
    retain_trusted(served, &KeySetTrust::default(), check)
}

/// [`retain_pinned`], also keeping a served key that a root-signed key set
/// authorises for this tree (#1454).
pub fn retain_trusted<F: FnMut(bool, String)>(
    served: Vec<(String, VerifyingKey)>,
    trust: &KeySetTrust,
    check: &mut F,
) -> Vec<(String, VerifyingKey)> {
    let pinned_ids = trusted_ids(trust);
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
pub fn require_pinned(
    doc: &crate::bundle::PublicKeyDoc,
    now_ms: u64,
) -> Result<crate::bundle::PublicKeyDoc, String> {
    require_pinned_with(doc, now_ms, &KeySetTrust::default())
}

/// [`require_pinned`], also accepting the signers a root-signed key set
/// authorises for this tree (#754, #1454). The key set only ever ADDS keys: the
/// compiled pin keeps working when the key set is absent, unreachable or
/// untrusted, so an old log (or a blocked fetch) never strands a verifier.
pub fn require_pinned_with(
    doc: &crate::bundle::PublicKeyDoc,
    now_ms: u64,
    trust: &KeySetTrust,
) -> Result<crate::bundle::PublicKeyDoc, String> {
    let pinned_ids = trusted_ids(trust);
    // Keep only the served keys that are genuinely pinned. Passing on the
    // unfiltered document would let a server list an attacker key beside the
    // pinned one: the pin check would pass on the pinned entry, and heads signed
    // by the attacker key would then verify under the unfiltered candidate set.
    let kept: Vec<crate::bundle::PublicKeyEntry> = doc
        .active_keys(now_ms)
        .into_iter()
        .filter_map(|e| {
            let vk = verifying_key_from_hex(&e.public_key).ok()?;
            let id = key_id_for(&vk);
            if pinned_ids.contains(&id) {
                Some(crate::bundle::PublicKeyEntry { key_id: id, ..e })
            } else {
                None
            }
        })
        .collect();
    let Some(first) = kept.first() else {
        return Err("served public_key.json does not match the pinned log key — refusing to trust \
             the served log"
            .to_string());
    };
    Ok(crate::bundle::PublicKeyDoc {
        public_key: first.public_key.clone(),
        keys: kept,
    })
}

/// Recomputed key ids of everything this verifier trusts for one tree: the
/// compiled pin (plus the operator/dev allowances in [`pinned_candidates`]) and
/// the signers a verified key set adds. A malformed key-set entry drops out.
fn trusted_ids(trust: &KeySetTrust) -> std::collections::HashSet<String> {
    let mut ids: std::collections::HashSet<String> = pinned_candidates()
        .iter()
        .map(|(_, vk)| key_id_for(vk))
        .collect();
    ids.extend(
        trust
            .signers
            .iter()
            .filter_map(|hex| verifying_key_from_hex(hex).ok())
            .map(|vk| key_id_for(&vk)),
    );
    ids
}

// ── Root-signed key sets (#754, wired at runtime by #1454) ────────────────────

/// The pinned OFFLINE root(s) that vouch for signer keys, as
/// `(public key hex, not_after_ms)`.
///
/// Mirrors `pollis_core::commands::transparency::PINNED_LOG_ROOT_KEYS` (the
/// client's pin, and the copy CI reads before publishing a key set). The two
/// must stay identical; a pollis-core test enforces it.
pub const PINNED_LOG_ROOT_KEYS: &[(&str, Option<u64>)] = &[(
    "c415c83da3d9035a0cd86087d7f4850299e45213f4e28d6b979c791165fdeeb4ca8430eb91c8106f65075649455525a902ce1b10a661d7129d6aa7d25d99ab5033fdadf902d89fc9a5d5740f46c2665e33868336e4002cca6d94359eca45d4dd83e0f6509986d3959fe0a0990ab8b5f7dd6c60b4def0c2a3122e03f654332db10c41d1b9e19d88d659add64f9b3485687bd7fab5fe0b01242f9d74d0ebfa8d5436b3180f72cfdf78873ec5d04251ed25f898b37db5c9b71e0bf1bd15c1cf71e84db053462b55c80870f8aec1351196b6c0459fd5e1683d00d1a27f33b21b195bd0e3238d76ea9f613b46556e3b86a6df71e64af8a0a78327fe248f158a902152413b1d6e0e8bfb43b0186d4b34b34dc3e71c6bacc6770e88ac52f5f5989b550025e016091efa3245fedd7b12997288b0b3e1f2b01e0924f2c80044198ad90a6f595a0d06ffe851f33bd4487f293c6a63970c2432c538180700495ba755bdbaf525fce59f6c75fcfbced8e0df1b16e00c185438d437cd4fc6fe8aa1dccc52dc6757958766ecce9064a19ff6f441cb823601cc20a2bf27e0b59499f7e4a5d5c0ed117012ede65982ffc160d20256eb42f5eaa06230a91b999afaa4514b74fb2896d049251d912016c5dae3767f3a08f079ce96ce34dbf7a8bbd677a9008ebcd45f214438788db8dfde3276ff2de38a42217025f3e86da8afe2278357f0fd80ae10c010e1f1bf74acb774f3bfe8d26cefe9004e16d7ee2267ee0358664853c0d5ae71c8f9a0e5376cee3ccb5f0d956f7db7253ce1f035bd446bda49dbf3461aae23ce685a4c7d4dfb9c14429685e55d84dc0f819f0bcb3ee7b02b4901f8c2fb87f3bd10606096716fb9862787717e2e3c3930340ce7d9c2a8bd591297130f55edb6398e6b8243012b22dd8d7ff9937c651bca6cb45373a433b500f9a219d488ac2320742e89128f0799a834e5e01dacf500e2b0998954edc87dfffc3cf85bebe6bca9608a34e4efc4092de97b2ddef75a2669060fe2522ad5ac8157d15e0e983055145f824b4a4705a6a5a6952d6c3b234932505d9274157bed8aa77cfa836e63e79f41a881da33257dec31b5aa85be39746310db532b65639bae569e56fbbf05019bdb8ab1f7ef0cdc6adb3854d73534d78d96dcd6e1d9d357fa9a7ac5d90c5c4680c08e46cf3f9103c3b4aaa2e12115bf9d428e6754ec11430846025a4e03abf3df748e1cb446d518d0bb57584f94bf14dc407cce3321d2545b7d03d35f247149ca842d29796f6de0f0bb61b18143dbe1bac184eefaca43fad62abd7443eeb8d55f6640701e34373b1417c36bb0caee24762efc85f26eb4b369cdf9cb281d825ed924eb83b48d1f00586d0e893d0f87345b31bdb2efca8ae77200accc33f8292ea927af781f3de0f5ad761c1f3adb83d78067a79f78fb816e22877aa6caf565bfa6493de957b89d48c840018dcaa9856880648d55f6b341fe8caf5cb5c611ba6b5d0554c368017cac85bdd06989631ceb9d976bbdce89804584bc9b92678dee148631e1805ae1155798e9d6c2c223f27297fd1ca7510d14f9f4ccd47a032a0ffc02840cf84ee2286c7b5db863b8ca1ea86f08f29798a2be748ecf283ad5e11304c72a6254737b69c278951ae5b0611ac01576edda99fca114e2f5a3b4dc913d07bb7313a8a907b5544e22cfffd35166357a0e0cfafc70d1ef8f9eb22fc96bd3f00a165c0f64d0f01d2f050445570d7d292502ca37004633b401796d2372f1881d64bc86cf9722e4a34e8bd97e36fa129e58cec74fe87d7ef6b568549013d7aacff4086ccce0dfab996dcd613a7b920187f05efae94711972372aebd7a7bcea19e",
    None,
)];

/// What a served key set told us about who may sign a given tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySetOutcome {
    /// The statement verified under a pinned root; these signer keys (lowercase
    /// hex) are acceptable for the requested tree right now.
    Signers(Vec<String>),
    /// No live root is pinned, so the statement cannot be checked against
    /// anything. Withhold trust; never alarm.
    NoRootPinned,
    /// The statement did not verify under any live pinned root, is malformed, or
    /// has expired. An ALARM condition: something is serving a key set we cannot
    /// attribute to our root.
    Untrusted(String),
}

/// Derive the acceptable signer keys for `context` from a served key-set document.
///
/// Pure: no network, no clock (`now_ms` is passed in so expiry is testable).
/// `roots` are `(public key hex, not_after_ms)`; an expired root is ignored.
pub fn signers_from_key_set<'a>(
    document: &str,
    roots: impl IntoIterator<Item = (&'a str, Option<u64>)>,
    context: &[u8],
    now_ms: u64,
) -> KeySetOutcome {
    let live: Vec<&str> = roots
        .into_iter()
        .filter(|(_, not_after)| not_after.is_none_or(|exp| now_ms <= exp))
        .map(|(hex, _)| hex)
        .collect();
    if live.is_empty() {
        return KeySetOutcome::NoRootPinned;
    }

    let statement: verifiable_log::KeySetStatement = match serde_json::from_str(document) {
        Ok(s) => s,
        Err(e) => return KeySetOutcome::Untrusted(format!("key set is not readable: {e}")),
    };

    // A malformed pin is our bug, not the server's; it must not be read as the
    // server misbehaving, so it simply drops out of the candidates.
    let verified = live
        .iter()
        .filter_map(|hex| verifiable_log::root_key_from_hex(hex).ok())
        .any(|vk| statement.verify(&vk).is_ok());
    if !verified {
        return KeySetOutcome::Untrusted(
            "the served key set is not signed by any root this build pins".into(),
        );
    }

    let active = statement.active_signers_for(context, now_ms);
    if active.is_empty() {
        // Verified, but grants nothing: an expired statement, or one naming no
        // signer for this tree. Distinct from a forgery, and reported as such.
        return KeySetOutcome::Untrusted(
            "the key set verified but authorises no signer for this tree right now".into(),
        );
    }
    KeySetOutcome::Signers(
        active
            .iter()
            .map(|s| s.public_key.to_ascii_lowercase())
            .collect(),
    )
}

/// What the served key set contributes to trusting ONE tree, on top of the
/// compiled pin.
///
/// The default (no signers, no alarm) is exactly the pre-#1454 behaviour, and is
/// what an absent or unreachable key set resolves to: the compiled pin keeps
/// working when the fetch fails. A key set that IS served but cannot be
/// attributed to a pinned root (forged, malformed, expired, or naming no signer
/// for this tree) also falls back to the compiled pin, but carries an `alarm`
/// that every verdict built on it must surface. Never a silent accept.
///
/// The key set only ever ADDS signers. It cannot revoke a compiled pin: a
/// compiled key stays trusted until a release drops it or its `not_after` passes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeySetTrust {
    /// Signer keys (lowercase hex) the root authorises for this tree right now.
    pub signers: Vec<String>,
    /// Why a served key set was rejected, when it was.
    pub alarm: Option<String>,
}

impl KeySetTrust {
    /// Resolve a fetched key-set document (`None` = not published / not
    /// reachable) against `roots` for `context`. Pure.
    pub fn from_document<'a>(
        document: Option<&str>,
        roots: impl IntoIterator<Item = (&'a str, Option<u64>)>,
        context: &[u8],
        now_ms: u64,
    ) -> Self {
        let Some(doc) = document else {
            return Self::default();
        };
        match signers_from_key_set(doc, roots, context, now_ms) {
            KeySetOutcome::Signers(signers) => Self { signers, alarm: None },
            KeySetOutcome::NoRootPinned => Self::default(),
            KeySetOutcome::Untrusted(why) => Self {
                signers: Vec::new(),
                alarm: Some(why),
            },
        }
    }

    /// The violation a report carries when [`Self::alarm`] is set.
    pub fn alarm_violation(&self) -> Option<String> {
        self.alarm.as_ref().map(|why| {
            format!(
                "the log serves a key set this verifier cannot attribute to its pinned root \
                 ({why}); refusing to treat the log as verified"
            )
        })
    }
}

/// Fetch `{base}/v1/key-set.json`. Any failure (404 for a log published before
/// key sets or a self-hosted one without a root, transport, unreadable body) is
/// `None`: the compiled pin then stands alone, exactly as before #1454.
pub(crate) fn fetch_key_set_document(agent: &ureq::Agent, base: &str) -> Option<String> {
    let url = format!(
        "{}/{}/key-set.json",
        base.trim_end_matches('/'),
        crate::layout::API_VERSION
    );
    agent.get(&url).call().ok()?.into_string().ok()
}

/// Fetch the served key set and resolve it against the compiled roots
/// ([`PINNED_LOG_ROOT_KEYS`]) for `context`.
pub(crate) fn key_set_trust(agent: &ureq::Agent, base: &str, context: &[u8]) -> KeySetTrust {
    KeySetTrust::from_document(
        fetch_key_set_document(agent, base).as_deref(),
        PINNED_LOG_ROOT_KEYS.iter().copied(),
        context,
        crate::bundle::now_ms(),
    )
}

/// [`key_set_trust`] with an optional SOCKS5 `proxy`, for a caller that needs the
/// trust decision itself. pollis-core pin-checks the served key against it before
/// running the verifier, then hands the same value to the `verify_*_via_trusting`
/// entry points so the two cannot disagree. `Err` only for a malformed proxy URL,
/// which is never silently downgraded to a direct fetch.
pub fn key_set_trust_via(
    base: &str,
    proxy: Option<&str>,
    context: &[u8],
) -> crate::error::Result<KeySetTrust> {
    let agent = crate::remote::build_agent(proxy)?;
    Ok(key_set_trust(&agent, base, context))
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

    fn entry(vk: &VerifyingKey, not_after: Option<u64>) -> crate::bundle::PublicKeyEntry {
        crate::bundle::PublicKeyEntry {
            key_id: key_id_for(vk),
            algorithm: "ML-DSA-44".to_string(),
            public_key: hex::encode(vk.encode()),
            not_after,
        }
    }

    #[test]
    fn require_pinned_strips_an_unpinned_key_served_beside_the_pinned_one() {
        let foreign = foreign_vk();
        let pinned = pinned_vk();
        let doc = crate::bundle::PublicKeyDoc {
            public_key: hex::encode(foreign.encode()),
            keys: vec![entry(&foreign, None), entry(&pinned, Some(u64::MAX))],
        };
        let trusted = require_pinned(&doc, 0).expect("a pinned key is served");
        let ids: Vec<String> = trusted.verifying_candidates(0).into_iter().map(|(_, vk)| key_id_for(&vk)).collect();
        assert_eq!(ids, vec![key_id_for(&pinned)], "only the pinned key may survive");
        assert_eq!(trusted.public_key, hex::encode(pinned.encode()));
    }

    #[test]
    fn require_pinned_refuses_a_doc_with_no_pinned_key() {
        let foreign = foreign_vk();
        let doc = crate::bundle::PublicKeyDoc {
            public_key: hex::encode(foreign.encode()),
            keys: vec![entry(&foreign, None)],
        };
        assert!(require_pinned(&doc, 0).is_err());
    }

    #[test]
    fn require_pinned_drops_an_expired_pinned_overlap_key() {
        let foreign = foreign_vk();
        let pinned = pinned_vk();
        let doc = crate::bundle::PublicKeyDoc {
            public_key: hex::encode(foreign.encode()),
            keys: vec![entry(&foreign, None), entry(&pinned, Some(10))],
        };
        assert!(require_pinned(&doc, 11).is_err(), "an expired pinned key must not vouch for the doc");
    }

    #[test]
    fn refuses_when_only_foreign_keys_are_served() {
        let (kept, ok) = run(vec![("k".to_string(), foreign_vk())]);
        assert!(kept.is_empty());
        assert!(!ok);
    }

    // ── Root-signed key sets (#1454) ─────────────────────────────────────────

    const NOW: u64 = 1_700_000_000_000;
    const ACCOUNT: &[u8] = verifiable_log::STH_CONTEXT_ACCOUNT_KEYS;

    fn sk(seed: u8) -> verifiable_log::SigningKey {
        verifiable_log::SigningKey::from_seed(&[seed; 32].into())
    }

    fn signer_entry(vk: &VerifyingKey, trees: &[&[u8]], not_after_ms: Option<u64>) -> verifiable_log::SignerEntry {
        verifiable_log::SignerEntry {
            key_id: key_id_for(vk),
            public_key: hex::encode(vk.encode()),
            trees: trees.iter().map(|t| String::from_utf8_lossy(t).to_string()).collect(),
            not_after_ms,
        }
    }

    /// A real statement signed by `root_seed`, so these exercise real signatures.
    fn key_set(root_seed: u8, signers: Vec<verifiable_log::SignerEntry>) -> String {
        let st = verifiable_log::KeySetStatement::create(&sk(root_seed), NOW, Some(NOW + 1_000_000), signers)
            .expect("create");
        serde_json::to_string(&st).expect("json")
    }

    fn root_hex(seed: u8) -> String {
        hex::encode(sk(seed).verifying_key().encode())
    }

    fn doc_for(vk: &VerifyingKey) -> crate::bundle::PublicKeyDoc {
        crate::bundle::PublicKeyDoc {
            public_key: hex::encode(vk.encode()),
            keys: vec![entry(vk, None)],
        }
    }

    fn trust(document: Option<&str>, root: &str) -> KeySetTrust {
        KeySetTrust::from_document(document, [(root, None)], ACCOUNT, NOW)
    }

    #[test]
    fn rotate_in_a_signer_only_the_key_set_names_is_accepted() {
        let rotated = sk(40).verifying_key();
        let ks = key_set(1, vec![signer_entry(&rotated, &[ACCOUNT], None)]);
        let t = trust(Some(&ks), &root_hex(1));
        assert_eq!(t.alarm, None);
        assert_eq!(t.signers, vec![hex::encode(rotated.encode())]);
        // Without the key set the compiled pin alone refuses it...
        assert!(require_pinned(&doc_for(&rotated), NOW).is_err());
        // ...with it, the same served document is trusted.
        let kept = require_pinned_with(&doc_for(&rotated), NOW, &t).expect("root-authorised signer");
        assert_eq!(kept.public_key, hex::encode(rotated.encode()));
    }

    #[test]
    fn a_key_outside_the_set_is_still_refused() {
        let rotated = sk(40).verifying_key();
        let outsider = sk(41).verifying_key();
        let ks = key_set(1, vec![signer_entry(&rotated, &[ACCOUNT], None)]);
        let t = trust(Some(&ks), &root_hex(1));
        assert!(require_pinned_with(&doc_for(&outsider), NOW, &t).is_err());
        let (kept, ok) = {
            let mut passed = None;
            let kept = retain_trusted(vec![("x".into(), outsider)], &t, &mut |ok, _| passed = Some(ok));
            (kept, passed.unwrap())
        };
        assert!(kept.is_empty() && !ok);
    }

    #[test]
    fn rotate_out_a_signer_the_set_drops_or_expires_stops_counting() {
        let kept_signer = sk(40).verifying_key();
        let retired = sk(42).verifying_key();
        // Omitted from the new statement.
        let ks = key_set(1, vec![signer_entry(&kept_signer, &[ACCOUNT], None)]);
        let t = trust(Some(&ks), &root_hex(1));
        assert!(require_pinned_with(&doc_for(&retired), NOW, &t).is_err());
        // Named but past its own not_after.
        let ks = key_set(
            1,
            vec![
                signer_entry(&kept_signer, &[ACCOUNT], None),
                signer_entry(&retired, &[ACCOUNT], Some(NOW - 1)),
            ],
        );
        let t = trust(Some(&ks), &root_hex(1));
        assert_eq!(t.signers.len(), 1);
        assert!(require_pinned_with(&doc_for(&retired), NOW, &t).is_err());
    }

    #[test]
    fn a_signer_for_another_tree_does_not_count_for_this_one() {
        let rotated = sk(40).verifying_key();
        let ks = key_set(1, vec![signer_entry(&rotated, &[verifiable_log::STH_CONTEXT_BINARIES], None)]);
        let t = trust(Some(&ks), &root_hex(1));
        assert!(t.signers.is_empty());
        assert!(t.alarm.is_some(), "a verified set naming no signer for this tree is surfaced");
        assert!(require_pinned_with(&doc_for(&rotated), NOW, &t).is_err());
    }

    #[test]
    fn a_forged_key_set_alarms_and_falls_back_to_the_compiled_pin() {
        let attacker = sk(40).verifying_key();
        // Signed by a root this verifier does not pin.
        let ks = key_set(66, vec![signer_entry(&attacker, &[ACCOUNT], None)]);
        let t = trust(Some(&ks), &root_hex(1));
        assert!(t.signers.is_empty());
        assert!(t.alarm.is_some());
        assert!(t.alarm_violation().is_some());
        assert!(require_pinned_with(&doc_for(&attacker), NOW, &t).is_err());
        // The compiled pin still works, so the alarm is raised on a verdict rather
        // than the verifier going dark.
        assert!(require_pinned_with(&doc_for(&pinned_vk()), NOW, &t).is_ok());
    }

    #[test]
    fn an_unreadable_or_expired_key_set_alarms() {
        assert!(trust(Some("{not json"), &root_hex(1)).alarm.is_some());
        let st = verifiable_log::KeySetStatement::create(
            &sk(1),
            NOW,
            Some(NOW - 1),
            vec![signer_entry(&sk(40).verifying_key(), &[ACCOUNT], None)],
        )
        .unwrap();
        let t = trust(Some(&serde_json::to_string(&st).unwrap()), &root_hex(1));
        assert!(t.signers.is_empty() && t.alarm.is_some());
    }

    #[test]
    fn no_key_set_or_no_root_keeps_the_compiled_pin_without_alarming() {
        assert_eq!(trust(None, &root_hex(1)), KeySetTrust::default());
        let ks = key_set(1, vec![signer_entry(&sk(40).verifying_key(), &[ACCOUNT], None)]);
        let none: [(&str, Option<u64>); 0] = [];
        assert_eq!(KeySetTrust::from_document(Some(&ks), none, ACCOUNT, NOW), KeySetTrust::default());
        // An expired root pin is no pin.
        let rh = root_hex(1);
        assert_eq!(
            KeySetTrust::from_document(Some(&ks), [(rh.as_str(), Some(NOW - 1))], ACCOUNT, NOW),
            KeySetTrust::default()
        );
        assert!(require_pinned_with(&doc_for(&pinned_vk()), NOW, &KeySetTrust::default()).is_ok());
    }

    /// The key set the production log publishes today verifies under the pinned
    /// root for every tree and names the compiled signer, so shipped releases keep
    /// verifying exactly as before once the verifier starts reading it.
    #[test]
    fn the_committed_key_set_verifies_under_the_pinned_root_for_every_tree() {
        let doc = include_str!("../../key-set.json");
        for (name, ctx) in verifiable_log::STH_CONTEXTS {
            let t = KeySetTrust::from_document(
                Some(doc),
                PINNED_LOG_ROOT_KEYS.iter().copied(),
                ctx,
                1_786_400_000_000,
            );
            assert_eq!(t.alarm, None, "{name}: {:?}", t.alarm);
            assert!(
                t.signers.iter().any(|s| s == PINNED_LOG_PUBLIC_KEYS_HEX[0]),
                "{name}: the compiled signer must be in the root-signed set"
            );
        }
    }
}
