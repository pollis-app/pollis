//! Remote verification: fetch the static read API over HTTP and verify the log
//! trusting only the pinned key. Pure transport and orchestration over
//! [`verifiable_log`]'s checks, driven by the `/v1/index.json` manifest.

use std::collections::BTreeMap;

use verifiable_log::{
    is_equivocation, proof, ConsistencyProof, Entry, InclusionProof, ResetBody, ResetStatement,
    Sth, UniqueDataInvariant, VerifiableLog,
};
use verifiable_log_builder::account_key::{self, AccountKeyInvariant};
use verifiable_log_builder::binaries::{self, BinaryInvariant};

use crate::account::ACCOUNT_TENANT;
use crate::bundle::{
    now_ms, short, AccountManifest, BinaryManifest, Manifest, PublicKeyDoc, FORMAT_VERSION,
    MIN_LEAF_FORMAT_VERSION,
};
use crate::error::{Result, ServeError};
use crate::layout::{COMMIT_LOG_2_API_PREFIX, RESET_STATEMENT_PATH};
use crate::release::BINARIES_TENANT;

/// Just `format_version`, read before any full deserialize so a future shape
/// change reports as version skew, not a parse error that reads like tampering.
#[derive(serde::Deserialize)]
struct FormatProbe {
    #[serde(default)]
    format_version: u32,
}

/// Gate a served manifest on the [`FORMAT_VERSION`] ceiling before trusting
/// anything in it: newer is [`ServeError::VersionSkew`], older is accepted. The
/// trees are republished together, so gating the commit-log manifest covers the
/// bundle. Leaf-decoding paths use [`gate_leaf_format_version`] instead.
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

/// [`gate_format_version`] plus the [`MIN_LEAF_FORMAT_VERSION`] floor, for paths
/// that decode commit-log leaf contents: an older log would otherwise yield a
/// confident empty `PASS` (#701), so it is [`ServeError::LogTooOld`].
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
/// check; `notes` are warnings that never flip it.
pub struct Report {
    pub ok: bool,
    pub checks: Vec<(bool, String)>,
    pub notes: Vec<String>,
    /// Tagged lines that never flip the verdict (`RETIRED`, `KNOWN` forks, #1579).
    pub info: Vec<(String, String)>,
}

impl Report {
    fn new() -> Self {
        Self {
            ok: true,
            checks: Vec::new(),
            notes: Vec::new(),
            info: Vec::new(),
        }
    }

    fn info(&mut self, tag: &str, message: impl Into<String>) {
        self.info.push((tag.to_string(), message.into()));
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
        for (tag, message) in &self.info {
            println!("{tag}  {message}");
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

/// Verify a log served at `base_url`, trusting only the pinned key. `Err` only
/// when a prerequisite fetch fails; every verification problem is a failed check,
/// so a tampered artifact yields `Ok(Report { ok: false, .. })`.
///
/// A log serving a reset statement or `commit-log-2` (#1579) has the retired
/// first tree checked against the statement (reported RETIRED, failing on any
/// head the statement doesn't describe) and the successor verified in full from
/// the genesis the statement names. Otherwise `/v1` is the live tree.
pub fn verify_remote(base_url: &str) -> Result<Report> {
    let base = base_url.trim_end_matches('/');
    let agent = build_agent(None)?;

    let mut report = Report::new();

    let reset_url = format!("{base}/{RESET_STATEMENT_PATH}");
    let successor_index_url = format!("{base}/{COMMIT_LOG_2_API_PREFIX}/index.json");
    let reset_body = fetch_text_opt(&agent, &reset_url)?;
    let successor_index = fetch_text_opt(&agent, &successor_index_url)?;

    if reset_body.is_none() && successor_index.is_none() {
        verify_first_commit_log_as_live(&agent, base, &mut report)?;
    } else {
        verify_reset_commit_log(
            &agent,
            base,
            reset_body.as_deref(),
            successor_index.as_deref(),
            &mut report,
        )?;
    }

    // Absent optional trees are a note; present but invalid ones fail.
    verify_account_tree(&agent, base, &mut report);

    verify_binaries_tree(&agent, base, &mut report);

    Ok(report)
}

/// The served key document at `pk_url`, reduced to the keys trusted for
/// `context`: the compiled pin plus root-authorised signers (H-1, #754, #1454).
/// A log serving its own key is left with none and fails closed; a served key
/// set that doesn't verify is a failed check, not a skip.
fn trusted_candidates(
    agent: &ureq::Agent,
    base: &str,
    pk_url: &str,
    context: &[u8],
    report: &mut Report,
) -> Result<Vec<(String, verifiable_log::VerifyingKey)>> {
    let pk_doc: PublicKeyDoc = fetch_json(agent, pk_url)?;
    let candidates = pk_doc.verifying_candidates(now_ms());
    if candidates.is_empty() {
        return Err(ServeError::BadBundle(format!(
            "{pk_url} has no usable key (all expired or malformed)"
        )));
    }
    let trust = crate::pinned::key_set_trust(agent, base, context);
    if let Some(v) = trust.alarm_violation() {
        report.check(false, v);
    }
    let candidates = crate::pinned::retain_trusted(candidates, &trust, &mut |ok, label| {
        report.check(ok, label);
    });
    if candidates.is_empty() {
        return Err(ServeError::BadBundle(
            "served public_key.json does not match the pinned log key — refusing to trust the served log".to_string(),
        ));
    }
    Ok(candidates)
}

/// Verify a log with no reset statement or successor: `/v1` is the live tree.
fn verify_first_commit_log_as_live(
    agent: &ureq::Agent,
    base: &str,
    report: &mut Report,
) -> Result<()> {
    // Gate the format before judging the key set: a future format could leave
    // `active_keys` empty, and that must read as skew, not tampering.
    let index_url = format!("{base}/v1/index.json");
    let index_body = fetch_text(agent, &index_url)?;
    gate_format_version(&index_body)?;
    let manifest: Manifest = serde_json::from_str(&index_body)
        .map_err(|e| ServeError::Http(format!("parse {index_url}: {e}")))?;
    let candidates = trusted_candidates(
        agent,
        base,
        &format!("{base}/v1/public_key.json"),
        verifiable_log::STH_CONTEXT_COMMIT_LOG,
        report,
    )?;
    verify_commit_tree(
        agent,
        &format!("{base}/v1"),
        "",
        verifiable_log::STH_CONTEXT_COMMIT_LOG,
        &manifest,
        &candidates,
        report,
    );
    Ok(())
}

/// Verify the reset statement, the retired first tree against it, and the
/// successor tree in full (#1579).
fn verify_reset_commit_log(
    agent: &ureq::Agent,
    base: &str,
    reset_body: Option<&str>,
    successor_index: Option<&str>,
    report: &mut Report,
) -> Result<()> {
    use verifiable_log::{STH_CONTEXT_COMMIT_LOG, STH_CONTEXT_COMMIT_LOG_2};

    let successor_manifest: Option<Manifest> = match successor_index {
        Some(body) => {
            gate_format_version(body)?;
            Some(serde_json::from_str(body).map_err(|e| {
                ServeError::Http(format!("parse {COMMIT_LOG_2_API_PREFIX}/index.json: {e}"))
            })?)
        }
        None => None,
    };

    let retired_candidates = trusted_candidates(
        agent,
        base,
        &format!("{base}/v1/public_key.json"),
        STH_CONTEXT_COMMIT_LOG,
        report,
    )?;
    let successor_candidates = match successor_manifest {
        Some(_) => Some(trusted_candidates(
            agent,
            base,
            &format!("{base}/{COMMIT_LOG_2_API_PREFIX}/public_key.json"),
            STH_CONTEXT_COMMIT_LOG_2,
            report,
        )?),
        None => None,
    };

    let statement: Option<ResetStatement> = match reset_body {
        None => {
            report.check(
                false,
                "commit-log: a commit-log-2 tree is served, but no reset statement retires the first commit-log tree",
            );
            None
        }
        Some(body) => match serde_json::from_str::<ResetStatement>(body) {
            Ok(st) => Some(st),
            Err(e) => {
                report.check(false, format!("commit-log reset statement is readable: {e}"));
                None
            }
        },
    };
    let mut verified: Option<&ResetBody> = None;
    if let Some(st) = &statement {
        let mut keys = retired_candidates.clone();
        keys.extend(successor_candidates.iter().flatten().cloned());
        match st.verify_any(&keys) {
            Ok(_) => {
                report.check(
                    true,
                    "commit-log reset statement: signature (reset context) and self-consistency",
                );
                verified = Some(&st.statement);
            }
            Err(e) => report.check(false, format!("commit-log reset statement: {e}")),
        }
    }
    if let Some(body) = verified {
        let names_this_log = body.retired.tree == "commit-log"
            && body.retired.sth_context.as_bytes() == STH_CONTEXT_COMMIT_LOG
            && body.retired.path == crate::layout::API_VERSION
            && body.successor.tree == "commit-log-2"
            && body.successor.sth_context.as_bytes() == STH_CONTEXT_COMMIT_LOG_2
            && body.successor.path == COMMIT_LOG_2_API_PREFIX;
        report.check(
            names_this_log,
            "commit-log reset statement: retires commit-log at /v1 in favour of commit-log-2 at /v1/commit-log-2",
        );
        verify_retired_commit_log(agent, base, body, &retired_candidates, report);
    }

    match (successor_manifest, successor_candidates) {
        (Some(manifest), Some(candidates)) => {
            let entries = verify_commit_tree(
                agent,
                &format!("{base}/{COMMIT_LOG_2_API_PREFIX}"),
                "commit-log-2: ",
                STH_CONTEXT_COMMIT_LOG_2,
                &manifest,
                &candidates,
                report,
            );
            if let Some(body) = verified {
                let genesis_ok = entries.first().is_some_and(|e| {
                    hex::encode(e.leaf_hash()) == body.successor.genesis_root_hash
                });
                report.check(
                    genesis_ok,
                    "commit-log-2: its first leaf is the genesis the reset statement names",
                );
            }
        }
        _ => report.check(
            false,
            "commit-log-2: the successor tree the reset statement names is not served",
        ),
    }
    Ok(())
}

/// Check the retired first commit-log tree against a verified reset statement,
/// failing on anything the statement does not describe.
fn verify_retired_commit_log(
    agent: &ureq::Agent,
    base: &str,
    body: &ResetBody,
    candidates: &[(String, verifiable_log::VerifyingKey)],
    report: &mut Report,
) {
    use verifiable_log::{HeadClass, STH_CONTEXT_COMMIT_LOG};
    let r = &body.retired;
    let last = r.last_consistent.tree_size;
    let head_url = |size: u64| format!("{base}/v1/sth/{size}.json");
    let signed = |sth: &Sth| sth.verify_any_with_context(candidates, STH_CONTEXT_COMMIT_LOG).is_some();

    report.info(
        "RETIRED",
        format!(
            "commit-log (/v1): retired by the reset statement issued {}; continued as commit-log-2",
            utc(body.issued_at_ms)
        ),
    );

    report.check(
        signed(&r.last_consistent),
        format!("commit-log (retired): last consistent head, size {last}, signature"),
    );
    let mut off_chain: Vec<u64> = Vec::new();
    for &size in &r.chain_sizes {
        let ok = match fetch_json::<Sth>(agent, &head_url(size)) {
            Ok(sth) => {
                sth.tree_size == size
                    && signed(&sth)
                    && body.classify(size, &sth.root_hash) == HeadClass::Chain
                    && (size != last || sth == r.last_consistent)
            }
            Err(_) => false,
        };
        if !ok {
            off_chain.push(size);
        }
    }
    report.check(
        off_chain.is_empty(),
        if off_chain.is_empty() {
            format!(
                "commit-log (retired): all {} served heads on the consistent chain are signed and extend each other, up to size {last}",
                r.chain_sizes.len()
            )
        } else {
            format!("commit-log (retired): chain heads not served as the reset statement lists them: sizes {off_chain:?}")
        },
    );

    for fork in &r.forks {
        let ok = match fetch_json::<Sth>(agent, &head_url(fork.tree_size)) {
            Ok(sth) => {
                sth.tree_size == fork.tree_size
                    && signed(&sth)
                    && matches!(body.classify(sth.tree_size, &sth.root_hash), HeadClass::ListedFork(_))
            }
            Err(_) => false,
        };
        if ok {
            report.info(
                "KNOWN",
                format!(
                    "commit-log (retired): forked head at size {} (root {}, signed {}), listed in the reset statement",
                    fork.tree_size,
                    short(&fork.root_hash),
                    utc(fork.timestamp)
                ),
            );
        } else {
            report.check(
                false,
                format!(
                    "commit-log (retired): the fork at size {} is not served as the reset statement lists it",
                    fork.tree_size
                ),
            );
        }
    }

    // Heads superseded before the incident are listed by root only.
    let mut superseded_bad: Vec<u64> = Vec::new();
    for old in &r.superseded {
        match fetch_json::<Sth>(agent, &head_url(old.tree_size)) {
            Ok(sth) if sth.root_hash == old.root_hash => {}
            _ => superseded_bad.push(old.tree_size),
        }
    }
    if superseded_bad.is_empty() {
        if !r.superseded.is_empty() {
            report.info(
                "KNOWN",
                format!(
                    "commit-log (retired): {} superseded heads from an earlier generation of the tree, listed in the reset statement",
                    r.superseded.len()
                ),
            );
        }
    } else {
        report.check(
            false,
            format!("commit-log (retired): superseded heads not served as listed: sizes {superseded_bad:?}"),
        );
    }

    // Every head the retired tree still advertises must be described; anything
    // else means the freeze was broken or the statement is incomplete.
    let mut advertised: Vec<u64> = Vec::new();
    match fetch_text(agent, &format!("{base}/v1/index.json"))
        .ok()
        .and_then(|b| serde_json::from_str::<AdvertisedHeads>(&b).ok())
    {
        Some(m) => {
            advertised.extend(m.sth_sizes);
            advertised.extend(m.latest_tree_size);
        }
        None => report.check(false, "commit-log (retired): fetch and parse /v1/index.json"),
    }
    advertised.sort_unstable();
    advertised.dedup();
    let mut unlisted: Vec<u64> = Vec::new();
    let mut heads: Vec<(String, Sth)> = Vec::new();
    for size in &advertised {
        match fetch_json::<Sth>(agent, &head_url(*size)) {
            Ok(sth) => heads.push((format!("size {size}"), sth)),
            Err(_) => unlisted.push(*size),
        }
    }
    match fetch_json::<Sth>(agent, &format!("{base}/v1/sth/latest.json")) {
        Ok(sth) => heads.push(("latest.json".to_string(), sth)),
        Err(e) => report.check(false, format!("commit-log (retired): fetch latest.json: {e}")),
    }
    let mut undescribed: Vec<String> = Vec::new();
    for (label, sth) in &heads {
        let described = match body.classify(sth.tree_size, &sth.root_hash) {
            HeadClass::Chain | HeadClass::ListedFork(_) => signed(sth),
            HeadClass::Superseded(_) => true,
            HeadClass::Unlisted => false,
        };
        if !described {
            undescribed.push(label.clone());
        }
    }
    undescribed.extend(unlisted.iter().map(|s| format!("size {s} (not served)")));
    report.check(
        undescribed.is_empty(),
        if undescribed.is_empty() {
            "commit-log (retired): every head index.json and latest.json advertise is one the reset statement describes".to_string()
        } else {
            format!(
                "commit-log (retired): heads NOT described by the reset statement: {}",
                undescribed.join(", ")
            )
        },
    );
}

/// The advertised heads of the frozen retired manifest; lenient on purpose.
#[derive(serde::Deserialize)]
struct AdvertisedHeads {
    #[serde(default)]
    sth_sizes: Vec<u64>,
    #[serde(default)]
    latest_tree_size: Option<u64>,
}

/// `YYYY-MM-DD HH:MM UTC` for a millisecond timestamp (proleptic Gregorian).
fn utc(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let (h, m) = ((secs % 86_400) / 3600, (secs % 3600) / 60);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02} UTC")
}

/// Verify one commit-log tree under `prefix` (heads, equivocation, entry replay,
/// inclusion, consistency) under `context`. Returns the entries.
fn verify_commit_tree(
    agent: &ureq::Agent,
    prefix: &str,
    label: &str,
    context: &[u8],
    manifest: &Manifest,
    candidates: &[(String, verifiable_log::VerifyingKey)],
    report: &mut Report,
) -> Vec<Entry> {
    let mut sths: BTreeMap<u64, Sth> = BTreeMap::new();
    for size in &manifest.sth_sizes {
        let url = format!("{prefix}/sth/{size}.json");
        match fetch_json::<Sth>(agent, &url) {
            Ok(sth) => {
                report.check(
                    sth.tree_size == *size,
                    format!("{label}STH[{size}] tree_size matches its URL"),
                );
                report.check(
                    sth.verify_any_with_context(candidates, context).is_some(),
                    format!("{label}STH[{size}] signature"),
                );
                sths.insert(*size, sth);
            }
            Err(e) => report.check(false, format!("{label}fetch STH[{size}]: {e}")),
        }
    }

    // A latest.json that disagrees with the newest head would silently strand
    // auditors following the moving pointer.
    if let Some(max_size) = manifest.latest_tree_size {
        match fetch_json::<Sth>(agent, &format!("{prefix}/sth/latest.json")) {
            Ok(latest) => {
                let agrees =
                    latest.tree_size == max_size && sths.get(&max_size) == Some(&latest);
                report.check(agrees, format!("{label}latest.json matches the newest STH"));
                report.check(
                    latest.verify_any_with_context(candidates, context).is_some(),
                    format!("{label}latest.json signature"),
                );
            }
            Err(e) => report.check(false, format!("{label}fetch latest.json: {e}")),
        }
    }

    // sths is keyed by size, so this catches equivocation across the whole set.
    let collected: Vec<&Sth> = sths.values().collect();
    for i in 0..collected.len() {
        for j in (i + 1)..collected.len() {
            report.check(
                !is_equivocation(collected[i], collected[j]),
                format!(
                    "{label}no equivocation between size {} and size {}",
                    collected[i].tree_size, collected[j].tree_size
                ),
            );
        }
    }

    // Cross-check per-entry files against the list, so tampering with either
    // artifact is caught.
    let entries: Vec<Entry> = match fetch_json(agent, &format!("{prefix}/entries.json")) {
        Ok(e) => e,
        Err(e) => {
            report.check(false, format!("{label}fetch entries.json: {e}"));
            Vec::new()
        }
    };
    report.check(
        entries.len() as u64 == manifest.entry_count,
        format!("{label}entries.json count matches manifest"),
    );

    let mut per_entry_ok = true;
    for (i, entry) in entries.iter().enumerate() {
        match fetch_json::<Entry>(agent, &format!("{prefix}/entries/{i}.json")) {
            Ok(per) if &per == entry => {}
            Ok(_) => per_entry_ok = false,
            Err(_) => per_entry_ok = false,
        }
    }
    if !entries.is_empty() {
        report.check(per_entry_ok, format!("{label}per-entry files match entries.json"));
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
        report.check(replay_ok, format!("{label}all entries satisfy tenant invariants"));

        if replay_ok {
            for (size, sth) in &sths {
                let matches = match log.root_at(*size as usize) {
                    Ok(root) => sth.root_bytes().map(|r| r == root).unwrap_or(false),
                    Err(_) => false,
                };
                report.check(matches, format!("{label}STH[{size}] root matches replayed entries"));
            }
        }
    }

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
            format!("{label}inclusion: leaf {} in size {}", r.leaf_index, r.tree_size),
        );
    }

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
            format!("{label}consistency: size {} -> size {}", r.first, r.second),
        );
    }
    entries
}

/// Verify the account-key subtree: signatures under the account context, replay
/// through [`AccountKeyInvariant`]. Absent is a note; present but invalid fails.
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

    // Same pin as the commit log (H-1).
    let candidates = match trusted_candidates(
        agent,
        base,
        &format!("{prefix}/public_key.json"),
        account_key::STH_CONTEXT,
        report,
    ) {
        Ok(vk) => vk,
        Err(e) => {
            report.check(false, format!("account-keys: {e}"));
            return;
        }
    };

    // A commit-log head presented here fails the account-context signature check.
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

/// Verify the binaries subtree: signatures under the binaries context, replay
/// through [`BinaryInvariant`]. Absent is a note; present but invalid fails.
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

    // Same pin as the commit log (H-1).
    let candidates = match trusted_candidates(
        agent,
        base,
        &format!("{prefix}/public_key.json"),
        binaries::STH_CONTEXT,
        report,
    ) {
        Ok(vk) => vk,
        Err(e) => {
            report.check(false, format!("binaries: {e}"));
            return;
        }
    };

    // A head of another tree presented here fails the binaries-context check.
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

/// A blocking HTTP agent, optionally dialed through a SOCKS5 `proxy` so the
/// overlay hides the client's IP (`docs/relay-overlay-design.md` §14.4). A
/// malformed proxy URL is `Err`, never a silent direct fetch that would leak it.
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

/// Blocking GET returning the raw body, for callers that gate on it before parsing.
pub(crate) fn fetch_text(agent: &ureq::Agent, url: &str) -> Result<String> {
    agent
        .get(url)
        .call()
        .map_err(|e| ServeError::Http(format!("GET {url}: {e}")))?
        .into_string()
        .map_err(|e| ServeError::Http(format!("read body {url}: {e}")))
}

/// Blocking GET + JSON parse; any failure is [`ServeError::Http`].
pub(crate) fn fetch_json<T: serde::de::DeserializeOwned>(agent: &ureq::Agent, url: &str) -> Result<T> {
    let body = agent
        .get(url)
        .call()
        .map_err(|e| ServeError::Http(format!("GET {url}: {e}")))?
        .into_string()
        .map_err(|e| ServeError::Http(format!("read body {url}: {e}")))?;
    serde_json::from_str(&body).map_err(|e| ServeError::Http(format!("parse {url}: {e}")))
}

/// Like [`fetch_text`], but a 404 is `Ok(None)` (not published).
pub(crate) fn fetch_text_opt(agent: &ureq::Agent, url: &str) -> Result<Option<String>> {
    match agent.get(url).call() {
        Ok(resp) => resp
            .into_string()
            .map(Some)
            .map_err(|e| ServeError::Http(format!("read body {url}: {e}"))),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(ServeError::Http(format!("GET {url}: {e}"))),
    }
}

/// Like [`fetch_json`], but a 404 is `Ok(None)`, so an unpublished optional tree
/// (a note) is distinct from a broken one (a failure).
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

// End-to-end check of the overlay seam (`docs/relay-overlay-design.md` §14.4)
// with an in-process relay, SOCKS5 shim and loopback origin.
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
        // The cert binds the classic leaf key but nothing verifies under it.
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
