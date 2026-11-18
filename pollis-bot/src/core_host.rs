//! The real [`BotHost`]: one `pollis-core` `AppState` per installation.
//!
//! Interim: pollis-core's bot host API (#1182 lane B) has not landed on this
//! branch yet, so every call refuses with a clear message. Replaced by the thin
//! adapter once it does.

use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Result};
use async_trait::async_trait;

use crate::host::{BotHost, OpenedInstallation, PairOutcome, PairTarget, PairingProgress, WakeFn};
use crate::installations::InstallationRecord;
use crate::secrets::SecretSlot;

pub struct CoreHost;

impl CoreHost {
    pub fn new() -> Result<Self> {
        Ok(CoreHost)
    }
}

const NOT_YET: &str = "this build of pollis-bot cannot reach pollis-core's bot API yet";

#[async_trait]
impl BotHost for CoreHost {
    async fn pair(
        &self,
        _data_dir: &Path,
        _target: PairTarget,
        _device_name: Option<String>,
        _progress: &(dyn Fn(PairingProgress) + Send + Sync),
    ) -> Result<PairOutcome> {
        bail!(NOT_YET)
    }

    async fn open(&self, _record: &InstallationRecord, _dir: &Path, _wake: WakeFn) -> Result<OpenedInstallation> {
        bail!(NOT_YET)
    }

    async fn secrets(&self, _record: &InstallationRecord, _dir: &Path) -> Result<Arc<dyn SecretSlot>> {
        bail!(NOT_YET)
    }
}
