//! The `pollis-bot app` subcommands (#2004, #2005).
//!
//! - `app install`: point an installation at an App, from a local directory or
//!   an `https://` URL. Nothing is copied: the approval binds a hash, not a
//!   location, and the runtime re-reads the artifact and checks it against the
//!   admin's approval before every job. Prints the App hash an admin approves.
//! - `app dev-key`: create (once) this developer's signing key and print its
//!   public half, which an admin approves for a development installation.
//! - `app sign`: sign a build with that key (`<component>.sig.json`), so a
//!   development installation runs it without a new approval.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use pollis_app_manifest::{Manifest, MANIFEST_FILE_NAME};
use pollis_core::commands::bots::app_approval::{developer_key_public, new_developer_seed, sign_dev_build};

use crate::app_approval::{fetch_artifact, ArtifactLocation, DEV_KEY_FILE};
use crate::installations::{InstallationRecord, InstalledApp, Registry};
use crate::log;

/// The component and manifest locations an `--artifact` names: a directory
/// holding `pollis-app.json` and exactly one `.wasm`, a URL ending in `.wasm`
/// (the manifest beside it), or a URL of the directory.
pub fn resolve_artifact(artifact: &str) -> Result<(ArtifactLocation, ArtifactLocation)> {
    if artifact.starts_with("http://") {
        bail!("{artifact}: only https:// URLs or local directories. Fix: serve the App over HTTPS");
    }
    if artifact.starts_with("https://") {
        let base = if artifact.ends_with(".wasm") {
            artifact.rsplit_once('/').map(|(b, _)| b.to_string()).unwrap_or_default()
        } else {
            artifact.trim_end_matches('/').to_string()
        };
        let component = if artifact.ends_with(".wasm") { artifact.to_string() } else { format!("{base}/app.wasm") };
        return Ok((ArtifactLocation(component), ArtifactLocation(format!("{base}/{MANIFEST_FILE_NAME}"))));
    }
    let dir = Path::new(artifact);
    let dir = dir.canonicalize().with_context(|| format!("{artifact}: no such directory"))?;
    let manifest = dir.join(MANIFEST_FILE_NAME);
    if !manifest.is_file() {
        bail!("{} has no {MANIFEST_FILE_NAME}. Fix: point --artifact at the directory your App build wrote", dir.display());
    }
    let components: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "wasm"))
        .collect();
    let [component] = components.as_slice() else {
        bail!(
            "{} must hold exactly one .wasm component (found {}). Fix: remove the others",
            dir.display(),
            components.len()
        );
    };
    Ok((component.clone().into(), manifest.into()))
}

/// `app install`: check the App loads, make it the installation's handler and
/// return the record and the App hash an admin approves.
pub async fn app_install(data_dir: &Path, installation_id: &str, artifact: &str) -> Result<(InstallationRecord, String)> {
    let registry = Registry::new(data_dir);
    let mut record = registry
        .get(installation_id)?
        .ok_or_else(|| anyhow!("no installation {installation_id} on this host. Fix: use an id from `pollis-bot status`"))?;
    let (component, manifest) = resolve_artifact(artifact)?;
    let fetched = fetch_artifact(&component, &manifest).await?;
    Manifest::from_json(&fetched.manifest_json).with_context(|| format!("checking {manifest}"))?;
    let hash = pollis_app_manifest::app_hash(&fetched.component, &fetched.manifest_json)?;
    // Compile it as `run` would, so an App the sandbox refuses is refused here.
    let host = pollis_app_host::AppHost::new(pollis_app_host::HostConfig::default())
        .map_err(|e| anyhow!("starting the App sandbox: {e}"))?;
    host.prepare(hash, &fetched.component)
        .map_err(|e| anyhow!("{component} does not load in the App sandbox: {e}"))?;
    record.app = Some(InstalledApp { component, manifest });
    registry.save(&record)?;
    let hex = pollis_app_manifest::hex(&hash);
    log::info(&log::scope(installation_id), format!("App set; App hash {hex}"));
    Ok((record, hex))
}

/// The default developer key file.
pub fn default_dev_key(data_dir: &Path) -> PathBuf {
    data_dir.join(DEV_KEY_FILE)
}

fn read_seed(path: &Path) -> Result<[u8; 32]> {
    let bytes = std::fs::read(path).with_context(|| format!("reading the developer key {}", path.display()))?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("{} is not a developer key (32 bytes)", path.display()))
}

/// `app dev-key`: the developer key's public half, creating the key first if
/// the file does not exist. The file is the secret: keep it on your machine.
pub fn dev_key(path: &Path) -> Result<String> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let seed = new_developer_seed();
        write_private(path, &seed)?;
    }
    Ok(developer_key_public(&read_seed(path)?))
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    f.write_all(bytes)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("creating {}", path.display()))
}

/// `app sign`: sign the build in a local artifact directory and write
/// `<component>.sig.json`. Returns the signature file and the App hash.
pub fn sign(artifact_dir: &Path, key: &Path) -> Result<(PathBuf, String)> {
    let (component, manifest) = resolve_artifact(&artifact_dir.display().to_string())?;
    let component_bytes = std::fs::read(&component.0).with_context(|| format!("reading {component}"))?;
    let manifest_bytes = std::fs::read(&manifest.0).with_context(|| format!("reading {manifest}"))?;
    Manifest::from_json(&manifest_bytes)?;
    let hash = pollis_app_manifest::app_hash(&component_bytes, &manifest_bytes)?;
    let sig = sign_dev_build(&read_seed(key)?, &hash);
    let out = PathBuf::from(component.signature().0);
    std::fs::write(&out, serde_json::to_vec_pretty(&sig)?).with_context(|| format!("writing {}", out.display()))?;
    Ok((out, pollis_app_manifest::hex(&hash)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_artifact_names_the_component_and_the_manifest_beside_it() {
        let (c, m) = resolve_artifact("https://apps.example/triage/v1/triage.wasm").unwrap();
        assert_eq!(c.0, "https://apps.example/triage/v1/triage.wasm");
        assert_eq!(m.0, "https://apps.example/triage/v1/pollis-app.json");
        let (c, m) = resolve_artifact("https://apps.example/triage/v1/").unwrap();
        assert_eq!(c.0, "https://apps.example/triage/v1/app.wasm");
        assert_eq!(m.0, "https://apps.example/triage/v1/pollis-app.json");
        assert!(resolve_artifact("http://apps.example/a.wasm").is_err());
    }

    #[test]
    fn a_dev_key_is_made_once_and_signs_builds() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("dev.key");
        let public = dev_key(&key).unwrap();
        assert_eq!(dev_key(&key).unwrap(), public, "the same key on the second call");
        let artifact = dir.path().join("dist");
        std::fs::create_dir(&artifact).unwrap();
        std::fs::write(artifact.join("app.wasm"), b"\0asm\x0d\0\x01\0").unwrap();
        std::fs::write(artifact.join(MANIFEST_FILE_NAME), br#"{"name":"a","version":"1"}"#).unwrap();
        let (sig, _) = sign(&artifact, &key).unwrap();
        let parsed: pollis_core::commands::bots::app_approval::DevBuildSignature =
            serde_json::from_slice(&std::fs::read(sig).unwrap()).unwrap();
        assert_eq!(parsed.developer_key, public);
    }
}
