//! Remote verification: fetch the static read API over HTTP, driven by the
//! `index.json` manifest, and verify the whole log trusting only the pinned key.
//! Transport + orchestration only; all crypto is [`verifiable_log`]'s.

use std::collections::BTreeMap;

use verifiable_log::{
    is_equivocation, proof, ConsistencyProof, Entry, InclusionProof, Sth, UniqueDataInvariant,
    VerifiableLog,
};
use verifiable_log_builder::account_key::{self, AccountKeyInvariant};
use verifiable_log_builder::binaries::{self, BinaryInvariant};

use crate::account::ACCOUNT_TENANT;
use crate::bundle::{
    now_ms, AccountManifest, BinaryManifest, Manifest, PublicKeyDoc, FORMAT_VERSION,
    MIN_LEAF_FORMAT_VERSION,
};
use crate::error::{Result, ServeError};
use crate::release::BINARIES_TENANT;

/// Just `format_version`, read before any full deserialize so a breaking change
/// reports as skew rather than a parse error that reads like tampering.
#[derive(serde::Deserialize)]
struct FormatProbe {
    #[serde(default)]
    format_version: u32,
}

/// Gate a served manifest body on the [`FORMAT_VERSION`] ceiling before trusting
/// anything in it: newer is [`ServeError::VersionSkew`], older is accepted.
///
/// The three trees are republished together, so gating the commit-log manifest
/// covers the whole bundle. Leaf-decoding paths use [`gate_leaf_format_version`].
pub(crate) fn gate_format_version(body: &str) -> Result<()> {
    let probe: FormatProbe = serde_json::from_str(body)
        .map_err(|e| ServeError::Http(format!("read manifest format_version: {e}")))?;
    if probe.format_version > FORMAT_VERSION {
        return Err(ServeError::VersionSkew {
            served: probe.format_version,
            supported: FORMAT_VERSION,
        });
    }
    Ok(())
}

/// [`gate_format_version`] plus the [`MIN_LEAF_FORMAT_VERSION`] floor
/// ([`ServeError::LogTooOld`]), for paths that decode commit-log leaves.
pub(crate) fn gate_leaf_format_version(body: &str) -> Result<()> {
    // The ceiling reports first.
    let probe: FormatProbe = serde_json::from_str(body)
        .map_err(|e| ServeError::Http(format!("read manifest format_version: {e}")))?;
    if probe.format_version > FORMAT_VERSION {
        return Err(ServeError::VersionSkew {
            served: probe.format_version,
            supported: FORMAT_VERSION,
        });
    }
    if probe.format_version < MIN_LEAF_FORMAT_VERSION {
        return Err(ServeError::LogTooOld {
            served: probe.format_version,
            required: MIN_LEAF_FORMAT_VERSION,
        });
    }
    Ok(())
}

/// A pass/fail report over all remote checks. `ok` is the conjunction of every
/// check; `notes` are warnings that never flip `ok`.
pub struct Report {
    pub ok: bool,
    pub checks: Vec<(bool, String)>,
    pub notes: Vec<String>,
}

impl Report {
    fn new() -> Self {
        Self {
            ok: true,
            checks: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn check(&mut self, passed: bool, label: impl Into<String>) {
        if !passed {
            self.ok = false;
        }
        self.checks.push((passed, label.into()));
    }

    fn note(&mut self, message: impl Into<String>) {
        self.notes.push(message.into());
    }

    pub fn print(&self) {
        for (passed, label) in &self.checks {
            println!("{}  {}", if *passed { "PASS" } else { "FAIL" }, label);
        }
        for note in &self.notes {
            println!("NOTE  {note}");
        }
        if self.ok {
            println!("\nOK: all checks passed");
        } else {
            println!("\nFAILED: one or more checks did not pass");
        }
    }
}

/// Verify a log served at `base_url`, trusting only the pinned key.
///
/// `Err` only when a prerequisite (`public_key.json`, `index.json`) fails; a
/// tampered artifact yields `Ok(Report { ok: false, .. })`.
pub fn verify_remote(base_url: &str) -> Result<Report> {
    let base = base_url.trim_end_matches('/');
    let agent = build_agent(None)?;

    let mut report = Report::new();

    let pk_doc: PublicKeyDoc = fetch_json(&agent, &format!("{base}/v1/public_key.json"))?;

    // Gate on the wire format before parsing the manifest or judging the key
    // set: a future format could leave `active_keys` empty, and that must read as
    // skew, not as a bad bundle.
    let index_url = format!("{base}/v1/index.json");
    let index_body = fetch_text(&agent, &index_url)?;
    gate_format_version(&index_body)?;
    let manifest: Manifest = serde_json::from_str(&index_body)
        .map_err(|e| ServeError::Http(format!("parse {index_url}: {e}")))?;

    let candidates = pk_doc.verifying_candidates(now_ms());
    if candidates.is_empty() {
        return Err(ServeError::BadBundle(
            "public_key.json has no usable key (all expired or malformed)".to_string(),
        ));
    }
    // H-1: trust only the pinned key; a log serving its own key fails closed.
    let candidates = crate::pinned::retain_pinned(candidates, &mut |ok, label| {
        report.check(ok, label);
    });
    if candidates.is_empty() {
        return Err(ServeError::BadBundle(
            "served public_key.json does not match the pinned log key — refusing to trust the served log".to_string(),
        ));
    }

    // 1. Every advertised STH's signature, keyed by size for the proof checks.
    let mut sths: BTreeMap<u64, Sth> = BTreeMap::new();
    for size in &manifest.sth_sizes {
        let url = format!("{base}/v1/sth/{size}.json");
        match fetch_json::<Sth>(&agent, &url) {
            Ok(sth) => {
                report.check(
                    sth.tree_size == *size,
                    format!("STH[{size}] tree_size matches its URL"),
                );
                report.check(
                    sth.verify_any(&candidates).is_some(),
                    format!("STH[{size}] signature"),
                );
                sths.insert(*size, sth);
            }
            Err(e) => report.check(false, format!("fetch STH[{size}]: {e}")),
        }
    }

    // latest.json is the pointer clients follow; it must match the newest head.
    if let Some(max_size) = manifest.latest_tree_size {
        match fetch_json::<Sth>(&agent, &format!("{base}/v1/sth/latest.json")) {
            Ok(latest) => {
                let agrees =
                    latest.tree_size == max_size && sths.get(&max_size) == Some(&latest);
                report.check(agrees, "latest.json matches the newest STH");
                report.check(latest.verify_any(&candidates).is_some(), "latest.json signature");
            }
            Err(e) => report.check(false, format!("fetch latest.json: {e}")),
        }
    }

    // 2. Equivocation across every pair of heads.
    let collected: Vec<&Sth> = sths.values().collect();
    for i in 0..collected.len() {
        for j in (i + 1)..collected.len() {
            report.check(
                !is_equivocation(collected[i], collected[j]),
                format!(
                    "no equivocation between size {} and size {}",
                    collected[i].tree_size, collected[j].tree_size
                ),
            );
        }
    }

    // 3. Entries: cross-check per-entry files against the list (tampering with
    //    either is caught), replay through the invariants, confirm every root.
    let entries: Vec<Entry> = match fetch_json(&agent, &format!("{base}/v1/entries.json")) {
        Ok(e) => e,
        Err(e) => {
            report.check(false, format!("fetch entries.json: {e}"));
            Vec::new()
        }
    };
    report.check(
        entries.len() as u64 == manifest.entry_count,
        "entries.json count matches manifest",
    );

    let mut per_entry_ok = true;
    for (i, entry) in entries.iter().enumerate() {
        match fetch_json::<Entry>(&agent, &format!("{base}/v1/entries/{i}.json")) {
            Ok(per) if &per == entry => {}
            Ok(_) => per_entry_ok = false,
            Err(_) => per_entry_ok = false,
        }
    }
    if !entries.is_empty() {
        report.check(per_entry_ok, "per-entry files match entries.json");
    }

    if !entries.is_empty() {
        let mut log = VerifiableLog::new();
        for tenant in &manifest.enforce_unique {
            log.register_invariant(tenant.clone(), Box::new(UniqueDataInvariant));
        }
        let mut replay_ok = true;
        for entry in &entries {
            if log.append(entry.clone()).is_err() {
                replay_ok = false;
            }
        }
        report.check(replay_ok, "all entries satisfy tenant invariants");

        if replay_ok {
            for (size, sth) in &sths {
                let matches = match log.root_at(*size as usize) {
                    Ok(root) => sth.root_bytes().map(|r| r == root).unwrap_or(false),
                    Err(_) => false,
                };
                report.check(matches, format!("STH[{size}] root matches replayed entries"));
            }
        }
    }

    // 4. Inclusion proofs.
    for r in &manifest.inclusion {
        let url = format!(
            "{base}/v1/proof/inclusion/{}/{}.json",
            r.tree_size, r.leaf_index
        );
        let passed = match fetch_json::<InclusionProof>(&agent, &url) {
            Ok(p) => {
                let entry = entries.get(r.leaf_index as usize);
                let sth = sths.get(&r.tree_size);
                match (entry, sth) {
                    (Some(entry), Some(sth)) => proof::verify_inclusion_proof(entry, &p, sth),
                    _ => false,
                }
            }
            Err(_) => false,
        };
        report.check(
            passed,
            format!("inclusion: leaf {} in size {}", r.leaf_index, r.tree_size),
        );
    }

    // 5. Consistency proofs.
    for r in &manifest.consistency {
        let url = format!("{base}/v1/proof/consistency/{}-{}.json", r.first, r.second);
        let passed = match fetch_json::<ConsistencyProof>(&agent, &url) {
            Ok(p) => {
                let old = sths.get(&r.first);
                let new = sths.get(&r.second);
                match (old, new) {
                    (Some(o), Some(n)) => proof::verify_consistency_proof(o, n, &p),
                    _ => false,
                }
            }
            Err(_) => false,
        };
        report.check(
            passed,
            format!("consistency: size {} -> size {}", r.first, r.second),
        );
    }

    verify_account_tree(&agent, base, &mut report);
    verify_binaries_tree(&agent, base, &mut report);

    Ok(report)
}

/// A subtree's served key set narrowed to the pinned log key (H-1): the
/// subtrees are signed by the same key, so a server must not be able to swap in
/// its own key there either. `None`, with a failed check, when no usable pinned
/// key is served.
fn pinned_subtree_candidates(
    pk: &PublicKeyDoc,
    tree: &str,
    report: &mut Report,
) -> Option<Vec<(String, verifiable_log::VerifyingKey)>> {
    let served = pk.verifying_candidates(now_ms());
    if served.is_empty() {
        report.check(
            false,
            format!("{tree}: public_key.json has no usable key (all expired or malformed)"),
        );
        return None;
    }
    let kept = crate::pinned::retain_pinned(served, &mut |ok, label| {
        report.check(ok, format!("{tree}: {label}"));
    });
    if kept.is_empty() {
        return None;
    }
    Some(kept)
}

/// The commit-log checks for `{base}/v1/account-keys/...`, under the account
/// STH context and [`AccountKeyInvariant`]. An absent tree (404) is a note; a
/// present-but-invalid one is a hard failure.
fn verify_account_tree(agent: &ureq::Agent, base: &str, report: &mut Report) {
    let prefix = format!("{base}/v1/account-keys");

    let manifest: AccountManifest = match fetch_json_opt(agent, &format!("{prefix}/index.json")) {
        Ok(Some(m)) => m,
        Ok(None) => {
            report.note("account-keys tree not published (absent) — skipping account-tree checks");
            return;
        }
        Err(e) => {
            report.check(false, format!("account-keys: fetch index.json: {e}"));
            return;
        }
    };

    let candidates = match fetch_json::<PublicKeyDoc>(agent, &format!("{prefix}/public_key.json"))
    {
        Ok(pk) => match pinned_subtree_candidates(&pk, "account-keys", report) {
            Some(vk) => vk,
            None => return,
        },
        Err(e) => {
            report.check(false, format!("account-keys: fetch public_key.json: {e}"));
            return;
        }
    };

    // 1. Signatures under the account context: a commit-log head fails here.
    let mut sths: BTreeMap<u64, Sth> = BTreeMap::new();
    for size in &manifest.sth_sizes {
        let url = format!("{prefix}/sth/{size}.json");
        match fetch_json::<Sth>(agent, &url) {
            Ok(sth) => {
                report.check(
                    sth.tree_size == *size,
                    format!("account-keys: STH[{size}] tree_size matches its URL"),
                );
                report.check(
                    sth.verify_any_with_context(&candidates, account_key::STH_CONTEXT).is_some(),
                    format!("account-keys: STH[{size}] signature (account context)"),
                );
                sths.insert(*size, sth);
            }
            Err(e) => report.check(false, format!("account-keys: fetch STH[{size}]: {e}")),
        }
    }

    if let Some(max_size) = manifest.latest_tree_size {
        match fetch_json::<Sth>(agent, &format!("{prefix}/sth/latest.json")) {
            Ok(latest) => {
                let agrees =
                    latest.tree_size == max_size && sths.get(&max_size) == Some(&latest);
                report.check(agrees, "account-keys: latest.json matches the newest STH");
                report.check(
                    latest.verify_any_with_context(&candidates, account_key::STH_CONTEXT).is_some(),
                    "account-keys: latest.json signature (account context)",
                );
            }
            Err(e) => report.check(false, format!("account-keys: fetch latest.json: {e}")),
        }
    }

    // 2. Equivocation.
    let collected: Vec<&Sth> = sths.values().collect();
    for i in 0..collected.len() {
        for j in (i + 1)..collected.len() {
            report.check(
                !is_equivocation(collected[i], collected[j]),
                format!(
                    "account-keys: no equivocation between size {} and size {}",
                    collected[i].tree_size, collected[j].tree_size
                ),
            );
        }
    }

    // 3. Entries, replayed through the AccountKeyInvariant.
    let entries: Vec<Entry> = match fetch_json(agent, &format!("{prefix}/entries.json")) {
        Ok(e) => e,
        Err(e) => {
            report.check(false, format!("account-keys: fetch entries.json: {e}"));
            Vec::new()
        }
    };
    report.check(
        entries.len() as u64 == manifest.entry_count,
        "account-keys: entries.json count matches manifest",
    );

    let mut per_entry_ok = true;
    for (i, entry) in entries.iter().enumerate() {
        match fetch_json::<Entry>(agent, &format!("{prefix}/entries/{i}.json")) {
            Ok(per) if &per == entry => {}
            _ => per_entry_ok = false,
        }
    }
    if !entries.is_empty() {
        report.check(per_entry_ok, "account-keys: per-entry files match entries.json");
    }

    if !entries.is_empty() {
        // Stronger than the generic UniqueDataInvariant.
        let mut log = VerifiableLog::new();
        log.register_invariant(ACCOUNT_TENANT, Box::new(AccountKeyInvariant));
        let mut replay_ok = true;
        for entry in &entries {
            if log.append(entry.clone()).is_err() {
                replay_ok = false;
            }
        }
        report.check(replay_ok, "account-keys: all entries satisfy the account invariant");

        if replay_ok {
            for (size, sth) in &sths {
                let matches = match log.root_at(*size as usize) {
                    Ok(root) => sth.root_bytes().map(|r| r == root).unwrap_or(false),
                    Err(_) => false,
                };
                report.check(
                    matches,
                    format!("account-keys: STH[{size}] root matches replayed entries"),
                );
            }
        }
    }

    // 4. Inclusion proofs.
    for r in &manifest.inclusion {
        let url = format!("{prefix}/proof/inclusion/{}/{}.json", r.tree_size, r.leaf_index);
        let passed = match fetch_json::<InclusionProof>(agent, &url) {
            Ok(p) => {
                let entry = entries.get(r.leaf_index as usize);
                let sth = sths.get(&r.tree_size);
                match (entry, sth) {
                    (Some(entry), Some(sth)) => proof::verify_inclusion_proof(entry, &p, sth),
                    _ => false,
                }
            }
            Err(_) => false,
        };
        report.check(
            passed,
            format!("account-keys: inclusion: leaf {} in size {}", r.leaf_index, r.tree_size),
        );
    }

    // 5. Consistency proofs.
    for r in &manifest.consistency {
        let url = format!("{prefix}/proof/consistency/{}-{}.json", r.first, r.second);
        let passed = match fetch_json::<ConsistencyProof>(agent, &url) {
            Ok(p) => {
                let old = sths.get(&r.first);
                let new = sths.get(&r.second);
                match (old, new) {
                    (Some(o), Some(n)) => proof::verify_consistency_proof(o, n, &p),
                    _ => false,
                }
            }
            Err(_) => false,
        };
        report.check(
            passed,
            format!("account-keys: consistency: size {} -> size {}", r.first, r.second),
        );
    }
}

/// [`verify_account_tree`] for `{base}/v1/binaries/...`, under the binaries STH
/// context and [`BinaryInvariant`].
fn verify_binaries_tree(agent: &ureq::Agent, base: &str, report: &mut Report) {
    let prefix = format!("{base}/v1/binaries");

    let manifest: BinaryManifest = match fetch_json_opt(agent, &format!("{prefix}/index.json")) {
        Ok(Some(m)) => m,
        Ok(None) => {
            report.note("binaries tree not published (absent) — skipping binaries-tree checks");
            return;
        }
        Err(e) => {
            report.check(false, format!("binaries: fetch index.json: {e}"));
            return;
        }
    };

    let candidates = match fetch_json::<PublicKeyDoc>(agent, &format!("{prefix}/public_key.json"))
    {
        Ok(pk) => match pinned_subtree_candidates(&pk, "binaries", report) {
            Some(vk) => vk,
            None => return,
        },
        Err(e) => {
            report.check(false, format!("binaries: fetch public_key.json: {e}"));
            return;
        }
    };

    // 1. Signatures under the binaries context: another tree's head fails here.
    let mut sths: BTreeMap<u64, Sth> = BTreeMap::new();
    for size in &manifest.sth_sizes {
        let url = format!("{prefix}/sth/{size}.json");
        match fetch_json::<Sth>(agent, &url) {
            Ok(sth) => {
                report.check(
                    sth.tree_size == *size,
                    format!("binaries: STH[{size}] tree_size matches its URL"),
                );
                report.check(
                    sth.verify_any_with_context(&candidates, binaries::STH_CONTEXT).is_some(),
                    format!("binaries: STH[{size}] signature (binaries context)"),
                );
                sths.insert(*size, sth);
            }
            Err(e) => report.check(false, format!("binaries: fetch STH[{size}]: {e}")),
        }
    }

    if let Some(max_size) = manifest.latest_tree_size {
        match fetch_json::<Sth>(agent, &format!("{prefix}/sth/latest.json")) {
            Ok(latest) => {
                let agrees =
                    latest.tree_size == max_size && sths.get(&max_size) == Some(&latest);
                report.check(agrees, "binaries: latest.json matches the newest STH");
                report.check(
                    latest.verify_any_with_context(&candidates, binaries::STH_CONTEXT).is_some(),
                    "binaries: latest.json signature (binaries context)",
                );
            }
            Err(e) => report.check(false, format!("binaries: fetch latest.json: {e}")),
        }
    }

    // 2. Equivocation.
    let collected: Vec<&Sth> = sths.values().collect();
    for i in 0..collected.len() {
        for j in (i + 1)..collected.len() {
            report.check(
                !is_equivocation(collected[i], collected[j]),
                format!(
                    "binaries: no equivocation between size {} and size {}",
                    collected[i].tree_size, collected[j].tree_size
                ),
            );
        }
    }

    // 3. Entries, replayed through the BinaryInvariant.
    let entries: Vec<Entry> = match fetch_json(agent, &format!("{prefix}/entries.json")) {
        Ok(e) => e,
        Err(e) => {
            report.check(false, format!("binaries: fetch entries.json: {e}"));
            Vec::new()
        }
    };
    report.check(
        entries.len() as u64 == manifest.entry_count,
        "binaries: entries.json count matches manifest",
    );

    let mut per_entry_ok = true;
    for (i, entry) in entries.iter().enumerate() {
        match fetch_json::<Entry>(agent, &format!("{prefix}/entries/{i}.json")) {
            Ok(per) if &per == entry => {}
            _ => per_entry_ok = false,
        }
    }
    if !entries.is_empty() {
        report.check(per_entry_ok, "binaries: per-entry files match entries.json");
    }

    if !entries.is_empty() {
        let mut log = VerifiableLog::new();
        log.register_invariant(BINARIES_TENANT, Box::new(BinaryInvariant));
        let mut replay_ok = true;
        for entry in &entries {
            if log.append(entry.clone()).is_err() {
                replay_ok = false;
            }
        }
        report.check(replay_ok, "binaries: all entries satisfy the binary invariant");

        if replay_ok {
            for (size, sth) in &sths {
                let matches = match log.root_at(*size as usize) {
                    Ok(root) => sth.root_bytes().map(|r| r == root).unwrap_or(false),
                    Err(_) => false,
                };
                report.check(
                    matches,
                    format!("binaries: STH[{size}] root matches replayed entries"),
                );
            }
        }
    }

    // 4. Inclusion proofs.
    for r in &manifest.inclusion {
        let url = format!("{prefix}/proof/inclusion/{}/{}.json", r.tree_size, r.leaf_index);
        let passed = match fetch_json::<InclusionProof>(agent, &url) {
            Ok(p) => {
                let entry = entries.get(r.leaf_index as usize);
                let sth = sths.get(&r.tree_size);
                match (entry, sth) {
                    (Some(entry), Some(sth)) => proof::verify_inclusion_proof(entry, &p, sth),
                    _ => false,
                }
            }
            Err(_) => false,
        };
        report.check(
            passed,
            format!("binaries: inclusion: leaf {} in size {}", r.leaf_index, r.tree_size),
        );
    }

    // 5. Consistency proofs.
    for r in &manifest.consistency {
        let url = format!("{prefix}/proof/consistency/{}-{}.json", r.first, r.second);
        let passed = match fetch_json::<ConsistencyProof>(agent, &url) {
            Ok(p) => {
                let old = sths.get(&r.first);
                let new = sths.get(&r.second);
                match (old, new) {
                    (Some(o), Some(n)) => proof::verify_consistency_proof(o, n, &p),
                    _ => false,
                }
            }
            Err(_) => false,
        };
        report.check(
            passed,
            format!("binaries: consistency: size {} -> size {}", r.first, r.second),
        );
    }
}

/// A blocking HTTP agent, optionally dialing through a SOCKS5 `proxy` so the
/// overlay hides the client's IP from the transparency host
/// (`docs/relay-overlay-design.md` §14.4). A malformed proxy URL is `Err`, never
/// silently downgraded to a direct fetch that would leak the IP.
pub(crate) fn build_agent(proxy: Option<&str>) -> Result<ureq::Agent> {
    let mut builder =
        ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(30));
    if let Some(url) = proxy {
        let proxy = ureq::Proxy::new(url)
            .map_err(|e| ServeError::Http(format!("invalid overlay proxy {url}: {e}")))?;
        builder = builder.proxy(proxy);
    }
    Ok(builder.build())
}

/// Blocking GET returning the raw body, for the version gate to read first.
pub(crate) fn fetch_text(agent: &ureq::Agent, url: &str) -> Result<String> {
    agent
        .get(url)
        .call()
        .map_err(|e| ServeError::Http(format!("GET {url}: {e}")))?
        .into_string()
        .map_err(|e| ServeError::Http(format!("read body {url}: {e}")))
}

pub(crate) fn fetch_json<T: serde::de::DeserializeOwned>(agent: &ureq::Agent, url: &str) -> Result<T> {
    let body = agent
        .get(url)
        .call()
        .map_err(|e| ServeError::Http(format!("GET {url}: {e}")))?
        .into_string()
        .map_err(|e| ServeError::Http(format!("read body {url}: {e}")))?;
    serde_json::from_str(&body).map_err(|e| ServeError::Http(format!("parse {url}: {e}")))
}

/// [`fetch_json`], but a 404 is `Ok(None)` so an unpublished optional tree is
/// told apart from a broken one.
pub(crate) fn fetch_json_opt<T: serde::de::DeserializeOwned>(
    agent: &ureq::Agent,
    url: &str,
) -> Result<Option<T>> {
    match agent.get(url).call() {
        Ok(resp) => {
            let body = resp
                .into_string()
                .map_err(|e| ServeError::Http(format!("read body {url}: {e}")))?;
            let value =
                serde_json::from_str(&body).map_err(|e| ServeError::Http(format!("parse {url}: {e}")))?;
            Ok(Some(value))
        }
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(ServeError::Http(format!("GET {url}: {e}"))),
    }
}

// End-to-end check of the overlay proxy seam: in-process relay + SOCKS5 shim +
// loopback HTTP origin.
#[cfg(test)]
mod overlay_proxy_tests {
    use super::*;

    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use ml_dsa::{Keypair, MlDsa44, SigningKey};
    use pollis_relay::circuit::{Hop, SingleHopFactory};
    use pollis_relay::client::ClientIdentity;
    use pollis_relay::proto::DeviceCertMaterial;
    use pollis_relay::server::{RelayConfig, RelayServer, RelayStats};
    use pollis_relay::{Allowlist, CircuitFactory, OverlayMode, OverlayShim, RoutingPolicy};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const ORIGIN_NAME: &str = "origin.test";

    fn identity() -> Arc<ClientIdentity> {
        // Nothing verifies under the classic leaf key, so a fixed pattern works.
        const DEVICE_ED_PUB: [u8; 32] = [11u8; 32];
        let device = SigningKey::<MlDsa44>::from_seed(&[11u8; 32].into());
        let account = SigningKey::<MlDsa44>::from_seed(&[12u8; 32].into());
        let cert = DeviceCertMaterial::mint(
            &account,
            "d_verify_test",
            &DEVICE_ED_PUB,
            &device.verifying_key().encode(),
            1,
            1_700_000_000,
        );
        Arc::new(ClientIdentity::new(
            "u_verify_test",
            "d_verify_test",
            DEVICE_ED_PUB,
            device,
            cert,
        ))
    }

    /// An in-process relay that allows `ORIGIN_NAME` and resolves it to loopback.
    fn spawn_relay() -> (Arc<dyn CircuitFactory>, Arc<RelayStats>) {
        let mut config = RelayConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            // The test origin listens on a random port; a bare host means 443.
            Allowlist::from_patterns([format!("{ORIGIN_NAME}:*")]),
        )
        .unwrap();
        config
            .resolve_overrides
            .insert(ORIGIN_NAME.to_string(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        let cert = config.server_cert();
        let stats = config.stats.clone();
        let (_task, addr) = RelayServer::spawn(config).unwrap();
        std::mem::forget(_task);
        let factory: Arc<dyn CircuitFactory> =
            Arc::new(SingleHopFactory::new(Hop::new(addr, cert), identity()));
        (factory, stats)
    }

    /// A loopback HTTP origin answering `body` and counting connections.
    async fn spawn_plain_http(body: &'static str) -> (SocketAddr, Arc<AtomicUsize>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                c.fetch_add(1, Ordering::Relaxed);
                let body = body.to_string();
                tokio::spawn(async move {
                    let mut byte = [0u8; 1];
                    let mut buf = Vec::new();
                    while let Ok(n) = sock.read(&mut byte).await {
                        if n == 0 {
                            break;
                        }
                        buf.push(byte[0]);
                        if buf.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        (addr, count)
    }

    fn key_doc(vk: &verifiable_log::VerifyingKey) -> PublicKeyDoc {
        let hex = hex::encode(vk.encode());
        PublicKeyDoc {
            public_key: hex.clone(),
            keys: vec![crate::bundle::PublicKeyEntry {
                key_id: verifiable_log::key_id_for(vk),
                algorithm: "ML-DSA-44".to_string(),
                public_key: hex,
                not_after: None,
            }],
        }
    }

    #[test]
    fn subtree_refuses_a_served_key_that_is_not_pinned() {
        use ml_dsa::Keypair as _;
        let foreign = verifiable_log::SigningKey::from_seed(&[7u8; 32].into()).verifying_key();
        let mut report = Report::new();
        assert!(pinned_subtree_candidates(&key_doc(&foreign), "account-keys", &mut report).is_none());
        assert!(!report.ok, "an unpinned subtree key must fail the audit");
    }

    #[test]
    fn subtree_keeps_the_pinned_key() {
        let pinned = verifiable_log::verifying_key_from_hex(crate::pinned::PINNED_LOG_PUBLIC_KEYS_HEX[0]).unwrap();
        let mut report = Report::new();
        let kept = pinned_subtree_candidates(&key_doc(&pinned), "binaries", &mut report).unwrap();
        assert_eq!(kept.len(), 1);
        assert!(report.ok);
    }

    #[test]
    fn build_agent_rejects_malformed_proxy() {
        assert!(build_agent(Some("socks6://127.0.0.1:9")).is_err());
        assert!(build_agent(Some("socks5://127.0.0.1:9050")).is_ok());
        assert!(build_agent(None).is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn build_agent_proxy_traverses_relay_none_goes_direct() {
        let (origin_addr, origin_conns) = spawn_plain_http(r#"{"ok":true}"#).await;
        let (factory, stats) = spawn_relay();
        let policy = RoutingPolicy::new(
            OverlayMode::Strict,
            Allowlist::from_patterns([ORIGIN_NAME]),
            Allowlist::default(),
        );
        let shim = OverlayShim::start(policy, factory).await.unwrap();
        let proxy = format!("socks5://{}", shim.socks_addr());

        // `ureq` is blocking, so run it off the runtime.
        let origin_port = origin_addr.port();
        let proxy_c = proxy.clone();
        let via_overlay: serde_json::Value = tokio::task::spawn_blocking(move || {
            let agent = build_agent(Some(&proxy_c)).expect("agent with valid proxy");
            fetch_json(&agent, &format!("http://{ORIGIN_NAME}:{origin_port}/x.json"))
        })
        .await
        .unwrap()
        .expect("proxied fetch reaches the origin through the relay");
        assert_eq!(via_overlay["ok"], serde_json::json!(true));
        assert_eq!(stats.dials(), 1, "the relay must have dialed the origin");
        assert_eq!(origin_conns.load(Ordering::Relaxed), 1);

        let direct: serde_json::Value = tokio::task::spawn_blocking(move || {
            let agent = build_agent(None).expect("proxy-less agent");
            fetch_json(&agent, &format!("http://127.0.0.1:{origin_port}/x.json"))
        })
        .await
        .unwrap()
        .expect("direct fetch reaches the origin");
        assert_eq!(direct["ok"], serde_json::json!(true));
        assert_eq!(stats.dials(), 1, "the direct fetch must NOT traverse the relay");
        assert_eq!(origin_conns.load(Ordering::Relaxed), 2);

        drop(shim);
    }
}
