//! Remote verification: fetch the static read API over HTTP and verify the log
//! trusting only the public key. This is the whole point of the serve layer —
//! anyone can audit the log over plain HTTP, with no special access.
//!
//! Every cryptographic check reuses slice 1's verifiers
//! ([`verifiable_log`]) — STH signatures, equivocation, entry/STH-root replay,
//! inclusion, and consistency. Nothing here reimplements Merkle, proof, or
//! signature logic; this module is purely transport + orchestration.
//!
//! Discovery is manifest-driven: we fetch `/v1/index.json` and let it tell us
//! which STH sizes, entries, and proofs exist, then fetch and check each.

use std::collections::BTreeMap;

use verifiable_log::{
    is_equivocation, proof, verifying_key_from_hex, ConsistencyProof, Entry, InclusionProof, Sth,
    UniqueDataInvariant, VerifiableLog,
};
use verifiable_log_builder::account_key::{self, AccountKeyInvariant};
use verifiable_log_builder::binaries::{self, BinaryInvariant};

use crate::account::ACCOUNT_TENANT;
use crate::bundle::{AccountManifest, BinaryManifest, Manifest, PublicKeyDoc};
use crate::error::{Result, ServeError};
use crate::release::BINARIES_TENANT;

/// A pass/fail report over all remote checks. `ok` is the conjunction of every
/// individual check; the labelled list is for human-readable output. `notes`
/// carries non-failing warnings (e.g. an optional tree that is absent) — they
/// are printed but do not flip `ok`.
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

    /// Record a non-failing warning — surfaced to the operator but it never
    /// flips the overall verdict.
    fn note(&mut self, message: impl Into<String>) {
        self.notes.push(message.into());
    }

    /// Print the per-check report and the overall verdict to stdout.
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

/// Verify a log served at `base_url` (e.g. `http://127.0.0.1:1234`), trusting
/// only the public key it publishes.
///
/// Prerequisite fetches (`public_key.json`, `index.json`) returning an error
/// abort with `Err` — without them there is nothing to verify. Every subsequent
/// problem (a bad signature, a tampered entry, a missing or forged proof) is
/// recorded as a failed check and folded into [`Report::ok`], so a tampered
/// artifact yields `Ok(Report { ok: false, .. })` rather than an error.
pub fn verify_remote(base_url: &str) -> Result<Report> {
    let base = base_url.trim_end_matches('/');
    let agent = build_agent();

    let mut report = Report::new();

    // Prerequisites: the public key we anchor trust in, and the manifest that
    // tells us what to fetch.
    let pk_doc: PublicKeyDoc = fetch_json(&agent, &format!("{base}/v1/public_key.json"))?;
    let verifying_key = verifying_key_from_hex(&pk_doc.public_key)?;
    let manifest: Manifest = fetch_json(&agent, &format!("{base}/v1/index.json"))?;

    // 1. Fetch every advertised STH and verify its signature. Keyed by size so
    //    later proof checks can look the right head up.
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
                    sth.verify(&verifying_key),
                    format!("STH[{size}] signature"),
                );
                sths.insert(*size, sth);
            }
            Err(e) => report.check(false, format!("fetch STH[{size}]: {e}")),
        }
    }

    // latest.json must agree with the largest published head — it is the moving
    // pointer clients follow, so a mismatch would silently strand auditors.
    if let Some(max_size) = manifest.latest_tree_size {
        match fetch_json::<Sth>(&agent, &format!("{base}/v1/sth/latest.json")) {
            Ok(latest) => {
                let agrees =
                    latest.tree_size == max_size && sths.get(&max_size) == Some(&latest);
                report.check(agrees, "latest.json matches the newest STH");
                report.check(latest.verify(&verifying_key), "latest.json signature");
            }
            Err(e) => report.check(false, format!("fetch latest.json: {e}")),
        }
    }

    // 2. Equivocation: any two heads at the same size with different roots.
    //    (sths is keyed by size so this catches it across the whole set.)
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

    // 3. Entries: fetch the ordered list, cross-check each per-entry file
    //    against it (so tampering with EITHER artifact is caught), then replay
    //    through the tenant invariants and confirm every STH root.
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

    // 4. Inclusion proofs: each entry is committed by the head at its size.
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

    // 5. Consistency proofs: the smaller tree is a prefix of the larger.
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

    // 6. Account-key tree (the second tenant). It is a fully separate tree under
    //    `/v1/account-keys/...` with its own STHs signed under the account
    //    domain context. Absent (404) → a warning, not a failure (the live
    //    domain has no account tree until this feature's first publish). Present
    //    but invalid → hard failure folded into `report.ok` exactly like the
    //    commit-log checks. The commit-log checks above are untouched either way.
    verify_account_tree(&agent, base, &mut report);

    // 7. Binaries tree (the third tenant, binary transparency). Same shape as the
    //    account tree: a fully separate tree under `/v1/binaries/...` with STHs
    //    signed under the binaries domain context. Absent (404) → a warning;
    //    present but invalid → a hard failure folded into `report.ok`.
    verify_binaries_tree(&agent, base, &mut report);

    Ok(report)
}

/// Verify the account-key subtree served under `{base}/v1/account-keys/...`,
/// mirroring the commit-log checks but verifying STH signatures under the
/// account domain context and replaying through [`AccountKeyInvariant`].
///
/// An absent tree (the index 404s) records a note and returns without touching
/// `report.ok`. Any present-but-invalid state (a bad signature, a tampered
/// entry, a missing/forged proof, a malformed index) is a failed check — a hard
/// failure, same as the commit log.
fn verify_account_tree(agent: &ureq::Agent, base: &str, report: &mut Report) {
    let prefix = format!("{base}/v1/account-keys");

    // The manifest is the entry point. A 404 means the tree is simply not
    // published yet — warn and skip. A transport/parse error on a present tree
    // is a real problem and fails.
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

    // The account tree's public key (the same key; published for a self-
    // contained subtree). Anchor trust in it before any signature check.
    let verifying_key = match fetch_json::<PublicKeyDoc>(agent, &format!("{prefix}/public_key.json"))
    {
        Ok(pk) => match verifying_key_from_hex(&pk.public_key) {
            Ok(vk) => vk,
            Err(e) => {
                report.check(false, format!("account-keys: public key parses: {e}"));
                return;
            }
        },
        Err(e) => {
            report.check(false, format!("account-keys: fetch public_key.json: {e}"));
            return;
        }
    };

    // 1. Fetch every advertised account STH and verify its signature UNDER THE
    //    ACCOUNT CONTEXT. A commit-log head presented here fails this check.
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
                    sth.verify_with_context(&verifying_key, account_key::STH_CONTEXT),
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
                    latest.verify_with_context(&verifying_key, account_key::STH_CONTEXT),
                    "account-keys: latest.json signature (account context)",
                );
            }
            Err(e) => report.check(false, format!("account-keys: fetch latest.json: {e}")),
        }
    }

    // 2. Equivocation across the account heads.
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

    // 3. Entries: cross-check per-entry files against the ordered list, replay
    //    through the AccountKeyInvariant (no dup / no regression), and confirm
    //    every STH root.
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
        // The account tree enforces the AccountKeyInvariant (strictly increasing
        // identity_version per user, no duplicate) — strictly stronger than the
        // commit log's uniqueness, so register it explicitly rather than the
        // generic UniqueDataInvariant.
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

/// Verify the binaries subtree served under `{base}/v1/binaries/...`, mirroring
/// the account-key checks but verifying STH signatures under the binaries domain
/// context and replaying through [`BinaryInvariant`] (no forked re-issue,
/// monotonic release tags, payload/signed pairing).
///
/// An absent tree (the index 404s) records a note and returns without touching
/// `report.ok`. Any present-but-invalid state (a bad signature, a tampered entry,
/// a missing/forged proof, a malformed index) is a failed check — a hard failure,
/// same as the other two trees.
fn verify_binaries_tree(agent: &ureq::Agent, base: &str, report: &mut Report) {
    let prefix = format!("{base}/v1/binaries");

    // The manifest is the entry point. A 404 means the tree is simply not
    // published yet — warn and skip. A transport/parse error on a present tree is
    // a real problem and fails.
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

    // The binaries tree's public key (the same key; published for a self-
    // contained subtree). Anchor trust in it before any signature check.
    let verifying_key = match fetch_json::<PublicKeyDoc>(agent, &format!("{prefix}/public_key.json"))
    {
        Ok(pk) => match verifying_key_from_hex(&pk.public_key) {
            Ok(vk) => vk,
            Err(e) => {
                report.check(false, format!("binaries: public key parses: {e}"));
                return;
            }
        },
        Err(e) => {
            report.check(false, format!("binaries: fetch public_key.json: {e}"));
            return;
        }
    };

    // 1. Fetch every advertised binaries STH and verify its signature UNDER THE
    //    BINARIES CONTEXT. A commit-log or account-key head presented here fails.
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
                    sth.verify_with_context(&verifying_key, binaries::STH_CONTEXT),
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
                    latest.verify_with_context(&verifying_key, binaries::STH_CONTEXT),
                    "binaries: latest.json signature (binaries context)",
                );
            }
            Err(e) => report.check(false, format!("binaries: fetch latest.json: {e}")),
        }
    }

    // 2. Equivocation across the binaries heads.
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

    // 3. Entries: cross-check per-entry files against the ordered list, replay
    //    through the BinaryInvariant, and confirm every STH root.
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
        // The binaries tree enforces the BinaryInvariant (no forked re-issue,
        // monotonic tags, payload/signed pairing) — register it explicitly rather
        // than the generic UniqueDataInvariant.
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

/// A blocking HTTP agent with a sane timeout, shared by every fetch path. The
/// serve layer only ever talks to a loopback static host, so no TLS is needed.
pub(crate) fn build_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .build()
}

/// Blocking GET + JSON parse. A non-2xx status, transport error, or malformed
/// body all map to [`ServeError::Http`].
pub(crate) fn fetch_json<T: serde::de::DeserializeOwned>(agent: &ureq::Agent, url: &str) -> Result<T> {
    let body = agent
        .get(url)
        .call()
        .map_err(|e| ServeError::Http(format!("GET {url}: {e}")))?
        .into_string()
        .map_err(|e| ServeError::Http(format!("read body {url}: {e}")))?;
    serde_json::from_str(&body).map_err(|e| ServeError::Http(format!("parse {url}: {e}")))
}

/// Like [`fetch_json`] but distinguishes a `404 Not Found` (the resource is
/// simply absent) from a real failure: a 404 returns `Ok(None)`, a 2xx returns
/// `Ok(Some(T))`, and any other status / transport / parse error returns `Err`.
/// Used to tell "the optional account tree is not published" (a warning) apart
/// from "the account tree is present but broken" (a hard failure).
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
