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

/// The `format_version` alone, pulled out of a served manifest *before* any full
/// deserialize. A future breaking change may add or rename fields a whole
/// [`Manifest`] can no longer parse; reading only this one field first means a
/// genuine version skew is reported as skew, not as a parse error that reads like
/// tampering. Absent (a pre-#701 manifest) → 0 via `serde(default)`.
#[derive(serde::Deserialize)]
struct FormatProbe {
    #[serde(default)]
    format_version: u32,
}

/// Gate a served manifest body on its wire [`FORMAT_VERSION`], **before** trusting
/// anything else in it.
///
/// A log published in a format *newer* than this build understands is
/// [`ServeError::VersionSkew`] — the verifier is too old, upgrade it — and is kept
/// deliberately distinct from a verification failure. A format at or below what we
/// support (including a legacy manifest with no `format_version` field → 0) is
/// accepted and verified normally, so this build stays backward-compatible with
/// the not-yet-republished log.
///
/// Applied to the commit-log manifest, which is the mandatory first fetch of both
/// `remote` and `group`. The three trees are republished together under one
/// bundle version, so gating that manifest covers the whole served bundle.
///
/// This is the **ceiling only**. Paths that decode leaf *contents* need the floor
/// as well — see [`gate_leaf_format_version`], which `group` uses instead.
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

/// [`gate_format_version`] plus the [`MIN_LEAF_FORMAT_VERSION`] **floor**, for the
/// paths that decode commit-log leaf contents rather than just checking bytes.
///
/// The ceiling alone is not enough for those paths. A verifier newer than the log
/// passes the ceiling, then re-derives windowed pseudonyms (#701) against leaves
/// that still carry raw ids, matches nothing, and reports `Found: no` with a
/// confident `PASS` for a conversation the manifest itself lists. Refusing with
/// [`ServeError::LogTooOld`] turns that silent wrong answer into an actionable
/// one, which is the whole point of having a version gate.
pub(crate) fn gate_leaf_format_version(body: &str) -> Result<()> {
    // One parse, then both bounds — the ceiling still reports first, exactly as
    // it did when this delegated to `gate_format_version` and re-parsed.
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
/// individual check; the labelled list is for human-readable output. `notes`
/// carries non-failing warnings (e.g. an optional tree that is absent) — they
/// are printed but do not flip `ok`.
pub struct Report {
    pub ok: bool,
    pub checks: Vec<(bool, String)>,
    pub notes: Vec<String>,
    /// Tagged information lines that never flip the verdict, e.g. `RETIRED` for
    /// the retired first commit-log tree and `KNOWN` for each fork the reset
    /// statement lists (#1579).
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

    /// Record a tagged information line. Never flips the verdict.
    fn info(&mut self, tag: &str, message: impl Into<String>) {
        self.info.push((tag.to_string(), message.into()));
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

/// Verify a log served at `base_url` (e.g. `http://127.0.0.1:1234`), trusting
/// only the pinned public key.
///
/// Prerequisite fetches (`public_key.json`, `index.json`) returning an error
/// abort with `Err` — without them there is nothing to verify. Every subsequent
/// problem (a bad signature, a tampered entry, a missing or forged proof) is
/// recorded as a failed check and folded into [`Report::ok`], so a tampered
/// artifact yields `Ok(Report { ok: false, .. })` rather than an error.
///
/// ## The commit-log reset (#1579)
///
/// A log that serves a reset statement at `/v1/commit-log-reset.json` has
/// retired its first commit-log tree (`/v1/...`) and continues it as
/// `commit-log-2` (`/v1/commit-log-2/...`). Then:
///
/// * the statement's signature is verified, under its own domain;
/// * the retired tree is reported as RETIRED, not failed: every head on its
///   consistent chain is still fetched and checked against the leaf hashes the
///   statement lists, and every listed fork is fetched and reported as known.
///   A head the retired tree serves that the statement does not describe is a
///   failure;
/// * `commit-log-2` is verified in full, exactly as the first tree used to be,
///   and its first leaf must be the genesis the statement names.
///
/// A log with neither file (a self-hosted or older log) is verified the old way:
/// the `/v1/...` commit-log tree is the live one.
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

/// The served key document at `pk_url`, reduced to the keys this verifier trusts
/// for `context`: the compiled pin plus any signer a root-signed key set
/// authorises (#754, #1454). H-1: trust ONLY pinned keys, never the served one; a
/// log serving its own key (equivocation / MITM) is left with no key and fails
/// closed. A served key set that does not verify under the pinned root is a
/// failed check, not a skip.
fn trusted_candidates(
    agent: &ureq::Agent,
    base: &str,
    pk_url: &str,
    context: &[u8],
    report: &mut Report,
) -> Result<Vec<(String, verifiable_log::VerifyingKey)>> {
    let pk_doc: PublicKeyDoc = fetch_json(agent, pk_url)?;
    // The set of keys any head may verify under, at the current wall clock: during
    // an overlap window both the new and the retiring key are published, so a head
    // signed by either verifies and a rotation stays invisible.
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

/// The pre-#1579 path: the commit-log tree directly under `/v1/...` is the live
/// one. Used for a log that serves no reset statement and no successor tree.
fn verify_first_commit_log_as_live(
    agent: &ureq::Agent,
    base: &str,
    report: &mut Report,
) -> Result<()> {
    // Read the manifest body once and gate on its wire format *first* — before
    // deserializing the full struct AND before judging the key set. A log newer
    // than we understand must surface as "upgrade your verifier" (VersionSkew),
    // never as a parse failure and never as "no usable key". The gate has to win
    // over the empty-candidates check: a future format bump that changes how keys
    // are published could leave `active_keys` empty, and reporting that as a bad
    // bundle would masquerade a version skew as tampering/misconfiguration.
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

/// The #1579 path: verify the reset statement, the retired first tree against
/// it, and the successor tree in full.
fn verify_reset_commit_log(
    agent: &ureq::Agent,
    base: &str,
    reset_body: Option<&str>,
    successor_index: Option<&str>,
    report: &mut Report,
) -> Result<()> {
    use verifiable_log::{STH_CONTEXT_COMMIT_LOG, STH_CONTEXT_COMMIT_LOG_2};

    // Gate the successor's manifest before trusting anything in it.
    let successor_manifest: Option<Manifest> = match successor_index {
        Some(body) => {
            gate_format_version(body)?;
            Some(serde_json::from_str(body).map_err(|e| {
                ServeError::Http(format!("parse {COMMIT_LOG_2_API_PREFIX}/index.json: {e}"))
            })?)
        }
        None => None,
    };

    // Keys for the retired tree: its own frozen key document, pin-filtered.
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

    // The reset statement.
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

    // The successor, in full.
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

/// Check the retired first commit-log tree against a verified reset statement.
///
/// Reports RETIRED and each listed fork as information, and fails on anything
/// the statement does not describe.
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

    // The consistent chain, head by head.
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

    // The forks the statement lists.
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

    // Heads superseded before the incident: listed by root only.
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

    // Everything the retired tree still advertises must be described. A head at
    // an unlisted size, or a listed size with another root, means the freeze was
    // broken or the statement is incomplete.
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

/// The two fields of a retired manifest the retired-tree check reads. Lenient on
/// purpose: the retired manifest is frozen, and only its advertised heads matter.
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

/// Verify one commit-log tree served under `prefix` (e.g. `{base}/v1` or
/// `{base}/v1/commit-log-2`): every advertised head, equivocation, entries and
/// replay, inclusion and consistency, all under `context`. Returns the entries.
fn verify_commit_tree(
    agent: &ureq::Agent,
    prefix: &str,
    label: &str,
    context: &[u8],
    manifest: &Manifest,
    candidates: &[(String, verifiable_log::VerifyingKey)],
    report: &mut Report,
) -> Vec<Entry> {
    // 1. Fetch every advertised STH and verify its signature. Keyed by size so
    //    later proof checks can look the right head up.
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

    // latest.json must agree with the largest published head — it is the moving
    // pointer clients follow, so a mismatch would silently strand auditors.
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

    // 2. Equivocation: any two heads at the same size with different roots.
    //    (sths is keyed by size so this catches it across the whole set.)
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

    // 3. Entries: fetch the ordered list, cross-check each per-entry file
    //    against it (so tampering with EITHER artifact is caught), then replay
    //    through the tenant invariants and confirm every STH root.
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

    // 4. Inclusion proofs: each entry is committed by the head at its size.
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

    // 5. Consistency proofs: the smaller tree is a prefix of the larger.
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
    let candidates = match fetch_json::<PublicKeyDoc>(agent, &format!("{prefix}/public_key.json"))
    {
        Ok(pk) => match Some(pk.verifying_candidates(now_ms())).filter(|c| !c.is_empty()) {
            Some(vk) => vk,
            None => {
                report.check(
                    false,
                    "account-keys: public_key.json has no usable key (all expired or malformed)"
                        .to_string(),
                );
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
    let candidates = match fetch_json::<PublicKeyDoc>(agent, &format!("{prefix}/public_key.json"))
    {
        Ok(pk) => match Some(pk.verifying_candidates(now_ms())).filter(|c| !c.is_empty()) {
            Some(vk) => vk,
            None => {
                report.check(
                    false,
                    "binaries: public_key.json has no usable key (all expired or malformed)"
                        .to_string(),
                );
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

/// A blocking HTTP agent with a sane timeout, shared by every fetch path.
///
/// `proxy` is the overlay seam: when `Some(url)` (e.g.
/// `socks5h://127.0.0.1:9050`) every fetch is dialed through that SOCKS5 proxy,
/// so an app running with the closed overlay on hides its real IP from the
/// first-party transparency host (design `docs/relay-overlay-design.md` §14.4);
/// when `None` the agent is byte-for-byte the pre-overlay one (a direct fetch to
/// the loopback/static host, no proxy). A malformed proxy URL is surfaced as an
/// `Err` — never silently downgraded to a direct fetch, which would leak the IP
/// the overlay is meant to hide.
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

/// Blocking GET returning the raw body text. Used where the caller needs the
/// bytes before deciding how to parse them — the version gate reads
/// `format_version` off the manifest body first, then deserializes the full
/// [`Manifest`] from the same string.
pub(crate) fn fetch_text(agent: &ureq::Agent, url: &str) -> Result<String> {
    agent
        .get(url)
        .call()
        .map_err(|e| ServeError::Http(format!("GET {url}: {e}")))?
        .into_string()
        .map_err(|e| ServeError::Http(format!("read body {url}: {e}")))
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

/// Like [`fetch_text`], but a `404 Not Found` is `Ok(None)`: the document is
/// simply not published. Any other failure is still an error.
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

// ── Overlay proxy plumbing ────────────────────────────────────────────────────
//
// The load-bearing proof for the transparency VERIFY overlay slice (design
// `docs/relay-overlay-design.md` §14.4): `build_agent(Some("socks5://<shim>"))`
// makes the blocking `ureq` fetch traverse the relay (the relay observes the
// dial), while `build_agent(None)` dials the origin directly. An in-process
// relay + SOCKS5 shim + a fake loopback HTTP origin is a full end-to-end check
// of the seam pollis-core wires the closed overlay into — no live network, all
// generated in-test. Mirrors the pattern in `pollis-core/src/net/overlay.rs`.
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

    /// A client identity carrying a device key + a valid offline cert chain —
    /// exactly what a real client presents to the relay.
    fn identity() -> Arc<ClientIdentity> {
        // The classic leaf key is only carried by the frame — the cert binds it,
        // nothing verifies under it — so a fixed pattern stands in for it.
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

    /// Spawn an in-process relay that allows `ORIGIN_NAME` and resolves it to
    /// loopback, returning its dial-through factory + the shared stats.
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
        // Leak the accept task: it lives for the whole (short) test process.
        std::mem::forget(_task);
        let factory: Arc<dyn CircuitFactory> =
            Arc::new(SingleHopFactory::new(Hop::new(addr, cert), identity()));
        (factory, stats)
    }

    /// A loopback plain-HTTP origin that answers every request with `body` and
    /// counts accepted connections.
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
                    // Drain the request head, then reply.
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

    /// A malformed proxy URL surfaces as an error — never a silent direct fetch,
    /// which would leak the IP the overlay is meant to hide.
    #[test]
    fn build_agent_rejects_malformed_proxy() {
        // An unsupported proxy scheme is rejected outright.
        assert!(build_agent(Some("socks6://127.0.0.1:9")).is_err());
        // A valid SOCKS5 proxy URL builds.
        assert!(build_agent(Some("socks5://127.0.0.1:9050")).is_ok());
        // The `None` (overlay-off) path is always fine.
        assert!(build_agent(None).is_ok());
    }

    /// `build_agent(Some(shim))` routes the blocking fetch THROUGH the relay
    /// (the relay records the dial); `build_agent(None)` dials the origin
    /// directly (the relay sees no dial).
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

        // Proxied: fetch the real name through the shim → relay → origin. `ureq`
        // is blocking, so run it off the runtime.
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

        // Direct: `None` dials the origin's loopback address itself; the relay
        // sees no further dial.
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
