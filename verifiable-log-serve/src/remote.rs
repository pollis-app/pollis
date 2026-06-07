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

use crate::bundle::{Manifest, PublicKeyDoc};
use crate::error::{Result, ServeError};

/// A pass/fail report over all remote checks. `ok` is the conjunction of every
/// individual check; the labelled list is for human-readable output.
pub struct Report {
    pub ok: bool,
    pub checks: Vec<(bool, String)>,
}

impl Report {
    fn new() -> Self {
        Self {
            ok: true,
            checks: Vec::new(),
        }
    }

    fn check(&mut self, passed: bool, label: impl Into<String>) {
        if !passed {
            self.ok = false;
        }
        self.checks.push((passed, label.into()));
    }

    /// Print the per-check report and the overall verdict to stdout.
    pub fn print(&self) {
        for (passed, label) in &self.checks {
            println!("{}  {}", if *passed { "PASS" } else { "FAIL" }, label);
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
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .build();

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

    Ok(report)
}

/// Blocking GET + JSON parse. A non-2xx status, transport error, or malformed
/// body all map to [`ServeError::Http`].
fn fetch_json<T: serde::de::DeserializeOwned>(agent: &ureq::Agent, url: &str) -> Result<T> {
    let body = agent
        .get(url)
        .call()
        .map_err(|e| ServeError::Http(format!("GET {url}: {e}")))?
        .into_string()
        .map_err(|e| ServeError::Http(format!("read body {url}: {e}")))?;
    serde_json::from_str(&body).map_err(|e| ServeError::Http(format!("parse {url}: {e}")))
}
