//! The soak's evidence, and the Markdown report it renders.

use crate::config::human;

#[derive(Clone, Debug)]
pub struct Proof {
    pub name: String,
    /// `None` = not applicable / skipped, with the reason in `evidence`.
    pub pass: Option<bool>,
    pub evidence: String,
}

impl Proof {
    pub fn check(name: &str, pass: bool, evidence: impl Into<String>) -> Self {
        Self { name: name.to_string(), pass: Some(pass), evidence: evidence.into() }
    }
    pub fn skip(name: &str, why: impl Into<String>) -> Self {
        Self { name: name.to_string(), pass: None, evidence: why.into() }
    }
    fn mark(&self) -> &'static str {
        match self.pass {
            Some(true) => "PASS",
            Some(false) => "FAIL",
            None => "skip",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct FileRun {
    pub label: String,
    pub kind: String,
    pub content_type: String,
    pub size: u64,
    pub over_cap: bool,
    pub upload_secs: Option<f64>,
    pub view_secs: Option<f64>,
    pub save_secs: Option<f64>,
    pub delete_secs: Option<f64>,
    pub file_id: Option<String>,
    pub proofs: Vec<Proof>,
}

impl FileRun {
    pub fn push(&mut self, p: Proof) {
        if p.pass == Some(false) {
            eprintln!("[vault-soak] FAIL {} — {}: {}", self.label, p.name, p.evidence);
        }
        self.proofs.push(p);
    }
    pub fn failed(&self) -> usize {
        self.proofs.iter().filter(|p| p.pass == Some(false)).count()
    }
    fn status_of(&self, prefix: &str) -> &'static str {
        let ps: Vec<&Proof> = self.proofs.iter().filter(|p| p.name.starts_with(prefix)).collect();
        if ps.is_empty() || ps.iter().all(|p| p.pass.is_none()) {
            "–"
        } else if ps.iter().any(|p| p.pass == Some(false)) {
            "FAIL"
        } else {
            "ok"
        }
    }
}

fn secs(s: Option<f64>) -> String {
    s.map(|s| format!("{s:.2}")).unwrap_or_else(|| "–".to_string())
}

fn rate(size: u64, s: Option<f64>) -> String {
    match s {
        Some(s) if s > 0.0 => format!("{:.1}", size as f64 / (1024.0 * 1024.0) / s),
        _ => "–".to_string(),
    }
}

pub fn render(header: &[(String, String)], files: &[FileRun], global: &[Proof]) -> String {
    let mut o = String::new();
    o.push_str("# Vault media soak report (#1491)\n\n");
    for (k, v) in header {
        o.push_str(&format!("- **{k}:** {v}\n"));
    }
    let failed: usize = files.iter().map(|f| f.failed()).sum::<usize>()
        + global.iter().filter(|p| p.pass == Some(false)).count();
    let passed: usize = files.iter().flat_map(|f| &f.proofs).filter(|p| p.pass == Some(true)).count()
        + global.iter().filter(|p| p.pass == Some(true)).count();
    let stored = files.iter().filter(|f| !f.over_cap).count();
    let stored_bytes: u64 = files.iter().filter(|f| !f.over_cap).map(|f| f.size).sum();
    o.push_str(&format!(
        "\n**Result: {}** — {passed} proofs passed, {failed} failed; {} files ({stored} stored, {} refused over the cap), {} stored.\n\n",
        if failed == 0 { "PASS" } else { "FAIL" },
        files.len(),
        files.len() - stored,
        human(stored_bytes),
    ));

    o.push_str("## Per file\n\n");
    o.push_str("Timings: upload = `upload_media` (seal + presign + PUT + register) + `send_vault_message`; view = `get_media_url` + loopback fetch (or the in-memory byte path); save = `save_media_to_path` (download + decrypt + verify + write). MiB/s over the plaintext size.\n\n");
    o.push_str("| file | kind | size | upload | upload s | up MiB/s | view s | save s | down MiB/s | delete s | transit | at rest | DS rows | local | transient | round trip | deleted |\n");
    o.push_str("|---|---|---:|---|---:|---:|---:|---:|---:|---:|---|---|---|---|---|---|---|\n");
    for f in files {
        o.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            f.label,
            f.kind,
            human(f.size),
            match (f.over_cap, f.status_of("upload")) {
                (true, "ok") => "refused: over cap",
                (_, st) => st,
            },
            secs(f.upload_secs),
            rate(f.size, f.upload_secs),
            secs(f.view_secs),
            secs(f.save_secs),
            rate(f.size, f.save_secs),
            secs(f.delete_secs),
            f.status_of("transit"),
            f.status_of("at rest"),
            f.status_of("DS"),
            f.status_of("local"),
            f.status_of("transient"),
            f.status_of("round trip"),
            f.status_of("delete"),
        ));
    }

    o.push_str("\n## Global proofs\n\n");
    for p in global {
        o.push_str(&format!("- **{}** {} — {}\n", p.mark(), p.name, p.evidence));
    }

    o.push_str("\n## Evidence per file\n");
    for f in files {
        o.push_str(&format!(
            "\n### {} — {} {} ({})\n\n",
            f.label,
            human(f.size),
            f.content_type,
            f.file_id.as_deref().map(|id| format!("file id {id}")).unwrap_or_else(|| "not stored".to_string())
        ));
        for p in &f.proofs {
            o.push_str(&format!("- **{}** {} — {}\n", p.mark(), p.name, p.evidence));
        }
    }
    o
}
