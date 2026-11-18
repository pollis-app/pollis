//! The sandbox the bot runtime runs Apps in (#2004).
//!
//! An App is a WebAssembly component targeting `pollis:app@0.1.0`
//! (`pollis-app-wit/wit/app.wit`). For each job the runtime's job loop calls
//! [`AppHost::run_job`], which:
//!
//! 1. computes the effective grants (manifest ∩ admin grants) and refuses
//!    grants wider than the manifest;
//! 2. refuses to deliver a trigger outside them (undeclared trigger kind,
//!    ungranted channel or event kind, the bot's own message);
//! 3. takes the compiled component from a cache keyed by App hash (compiling
//!    on a miss, and refusing any import other than `pollis:app/host` and the
//!    stubbed WASI 0.2 subset in [`wasi`](crate::check_import): empty
//!    environment, stdio to the runtime log, `exit` traps, host clocks and
//!    randomness, a filesystem with no preopens, and no sockets or wasi:http);
//! 4. instantiates it in a fresh `Store` with a memory cap, a CPU deadline
//!    (epoch interruption) and host-call caps, and calls one handler;
//! 5. returns the queued actions and state writes only if the handler returned
//!    ok. A trap, timeout, memory overrun, call-cap overrun or handler error
//!    returns a [`JobError`] and nothing else, so nothing is applied.
//!
//! Every host function checks its call against the grants first and reports
//! a refusal to the App as an error that names the manifest field.

mod egress;
mod http;
mod state;
mod types;
mod wasi;

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use pollis_app_manifest::{
    effective_grants, AdminGrants, EffectiveGrants, EventKind, Manifest, TriggerKind,
};
use sha2::{Digest, Sha256};
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store, Trap, UpdateDeadline};

pub use http::{is_public_ip, PublicHttpClient, PublicResolver};
pub use state::HOST_IMPORTS;
pub use types::*;
pub use wasi::check_import;

/// The WIT package this host implements.
pub const WIT_PACKAGE: &str = "pollis:app@0.1.0";

#[allow(clippy::all)]
mod bindings {
    wasmtime::component::bindgen!({
        path: "../pollis-app-wit/wit",
        world: "app",
        imports: { default: trappable },
    });
}

use bindings::{App, AppPre};
use state::{DeadlineHit, HostCallLimitHit, JobState, MemoryCapHit};

/// An App's hash (#2005 defines how it is computed). The cache key for its
/// compiled component.
pub type AppHash = [u8; 32];

/// How the host is set up. One per runtime process.
#[derive(Debug, Clone)]
pub struct HostConfig {
    pub limits: JobLimits,
    /// Compiled components kept in memory. Compilation is the expensive part;
    /// instantiation per job is cheap.
    pub cache_capacity: usize,
}

impl Default for HostConfig {
    fn default() -> Self {
        HostConfig {
            limits: JobLimits::default(),
            cache_capacity: 64,
        }
    }
}

/// One job's inputs.
pub struct Job<'a> {
    pub installation: &'a InstallationContext,
    pub app_hash: AppHash,
    pub component: &'a [u8],
    pub manifest: &'a Manifest,
    pub admin_grants: &'a AdminGrants,
    pub trigger: Trigger,
    pub services: Arc<dyn HostServices>,
}

struct CacheEntry {
    component_digest: [u8; 32],
    pre: AppPre<JobState>,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<AppHash, CacheEntry>,
    order: VecDeque<AppHash>,
}

/// The App sandbox. Cheap to share across threads; jobs may run concurrently
/// (the job loop serialises jobs per installation, not per host).
pub struct AppHost {
    engine: Engine,
    linker: Linker<JobState>,
    config: HostConfig,
    cache: Mutex<Cache>,
}

impl AppHost {
    pub fn new(config: HostConfig) -> Result<AppHost, JobError> {
        let mut wasm = Config::new();
        wasm.wasm_component_model(true);
        wasm.epoch_interruption(true);
        wasm.max_wasm_stack(1 << 20);
        let engine =
            Engine::new(&wasm).map_err(|e| JobError::InvalidComponent(format!("engine: {e}")))?;
        let mut linker = Linker::new(&engine);
        App::add_to_linker::<_, HasSelf<_>>(&mut linker, |state: &mut JobState| state)
            .map_err(|e| JobError::InvalidComponent(format!("linker: {e}")))?;
        wasi::add_to_linker(&mut linker)
            .map_err(|e| JobError::InvalidComponent(format!("linker: {e}")))?;
        Ok(AppHost {
            engine,
            linker,
            config,
            cache: Mutex::new(Cache::default()),
        })
    }

    pub fn limits(&self) -> &JobLimits {
        &self.config.limits
    }

    /// Compile (or fetch from the cache) the component for `app_hash`.
    /// Exposed so the runtime can validate an App when it is installed.
    pub fn prepare(&self, app_hash: AppHash, component: &[u8]) -> Result<(), JobError> {
        self.pre(app_hash, component).map(|_| ())
    }

    /// Whether a compiled component for this App hash is cached.
    pub fn is_cached(&self, app_hash: &AppHash) -> bool {
        self.cache
            .lock()
            .map(|c| c.entries.contains_key(app_hash))
            .unwrap_or(false)
    }

    fn pre(&self, app_hash: AppHash, component: &[u8]) -> Result<AppPre<JobState>, JobError> {
        let digest: [u8; 32] = Sha256::digest(component).into();
        {
            let cache = self
                .cache
                .lock()
                .map_err(|_| JobError::InvalidComponent("cache poisoned".into()))?;
            if let Some(entry) = cache.entries.get(&app_hash) {
                if entry.component_digest != digest {
                    return Err(JobError::AppHashMismatch(hex(&app_hash)));
                }
                return Ok(entry.pre.clone());
            }
        }
        let compiled = Component::new(&self.engine, component)
            .map_err(|e| JobError::InvalidComponent(format!("does not compile: {e}")))?;
        for (name, _) in compiled.component_type().imports(&self.engine) {
            wasi::check_import(name).map_err(JobError::ImportRefused)?;
        }
        let instance_pre = self.linker.instantiate_pre(&compiled).map_err(|e| {
            JobError::InvalidComponent(format!("does not match `{WIT_PACKAGE}`: {e}"))
        })?;
        let pre = AppPre::new(instance_pre).map_err(|e| {
            JobError::InvalidComponent(format!("does not export the `{WIT_PACKAGE}` handlers: {e}"))
        })?;
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| JobError::InvalidComponent("cache poisoned".into()))?;
        if !cache.entries.contains_key(&app_hash) {
            while cache.entries.len() >= self.config.cache_capacity.max(1) {
                let Some(oldest) = cache.order.pop_front() else {
                    break;
                };
                cache.entries.remove(&oldest);
            }
            cache.order.push_back(app_hash);
            cache.entries.insert(
                app_hash,
                CacheEntry {
                    component_digest: digest,
                    pre: pre.clone(),
                },
            );
        }
        Ok(pre)
    }

    /// Run one job: the App's handler for `job.trigger`, in a fresh instance.
    ///
    /// Blocks for up to the job deadline plus the duration of any host service
    /// call in flight; call it from a blocking thread.
    pub fn run_job(&self, job: Job<'_>) -> Result<JobOutcome, JobError> {
        job.admin_grants
            .check_not_wider(job.manifest)
            .map_err(|e| JobError::GrantsRefused(e.to_string()))?;
        let grants = effective_grants(job.manifest, job.admin_grants);
        check_delivery(&job.trigger, &grants, job.installation).map_err(JobError::NotDelivered)?;

        let pre = self.pre(job.app_hash, job.component)?;

        let limits = self.config.limits.clone();
        let deadline = Instant::now() + limits.deadline;
        let state = JobState::new(
            job.installation.clone(),
            grants,
            limits.clone(),
            job.services,
            deadline,
            &job.trigger,
        );
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limiter);
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(move |_| {
            if Instant::now() >= deadline {
                Err(wasmtime::Error::new(DeadlineHit))
            } else {
                // Another job's deadline bumped the shared epoch; keep going.
                Ok(UpdateDeadline::Continue(1))
            }
        });

        // One-shot watchdog: bumps the engine epoch once, at this job's
        // deadline, unless the job finishes first. No periodic ticker.
        let (done, wait) = mpsc::channel::<()>();
        let engine = self.engine.clone();
        let watchdog = std::thread::spawn(move || {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if let Err(RecvTimeoutError::Timeout) = wait.recv_timeout(remaining) {
                engine.increment_epoch();
            }
        });

        let result = pre
            .instantiate(&mut store)
            .and_then(|app| call_handler(&app, &mut store, &job.trigger));

        drop(done);
        let _ = watchdog.join();
        store.data_mut().flush_stdio();

        match result {
            Ok(Ok(())) => Ok(store.into_data().into_outcome()),
            Ok(Err(message)) => Err(JobError::HandlerFailed(message)),
            Err(error) => Err(classify(error, store.data(), &limits)),
        }
    }
}

fn call_handler(
    app: &App,
    store: &mut Store<JobState>,
    trigger: &Trigger,
) -> wasmtime::Result<Result<(), String>> {
    match trigger {
        Trigger::Message(m) => app.call_on_message(store, &state::to_wit_message(m)),
        Trigger::Mention(m) => app.call_on_mention(store, &state::to_wit_message(m)),
        Trigger::Command(c) => app.call_on_command(store, &state::to_wit_command(c)),
        Trigger::Schedule(s) => app.call_on_schedule(store, &state::to_wit_schedule(s)),
        Trigger::Request(r) => app.call_on_request(store, &state::to_wit_request(r)),
        Trigger::Webhook(w) => app.call_on_webhook(store, &state::to_wit_webhook(w)),
    }
}

fn classify(error: wasmtime::Error, state: &JobState, limits: &JobLimits) -> JobError {
    if state.wasi.exited {
        return JobError::Trap(wasi::ExitCalled.to_string());
    }
    if let Some(hit) = error.downcast_ref::<HostCallLimitHit>() {
        return JobError::HostCallLimit(hit.0.clone());
    }
    if error.downcast_ref::<DeadlineHit>().is_some()
        || matches!(error.downcast_ref::<Trap>(), Some(Trap::Interrupt))
    {
        return JobError::Timeout(limits.deadline);
    }
    if error.downcast_ref::<MemoryCapHit>().is_some() || state.limiter.exceeded {
        return JobError::MemoryExceeded(limits.memory_bytes);
    }
    if let Some(trap) = error.downcast_ref::<Trap>() {
        return JobError::Trap(trap.to_string());
    }
    JobError::Trap(error.to_string())
}

/// The trigger kind a trigger is delivered under.
pub fn trigger_kind(trigger: &Trigger) -> TriggerKind {
    match trigger {
        Trigger::Message(_) => TriggerKind::Messages,
        Trigger::Mention(_) => TriggerKind::Mentions,
        Trigger::Command(_) => TriggerKind::Commands,
        Trigger::Schedule(_) => TriggerKind::Schedule,
        Trigger::Request(_) => TriggerKind::Requests,
        Trigger::Webhook(_) => TriggerKind::Webhook,
    }
}

/// The message a trigger carries, if any.
pub fn trigger_message(trigger: &Trigger) -> Option<&Message> {
    match trigger {
        Trigger::Message(m) | Trigger::Mention(m) => Some(m),
        Trigger::Command(c) => Some(&c.message),
        _ => None,
    }
}

/// The event kind a message is seen as.
pub fn event_kind(message: &Message) -> EventKind {
    if message.edited_at.is_some() {
        EventKind::Edited
    } else {
        EventKind::Created
    }
}

/// Whether a trigger may reach the App at all. The job loop can call this
/// before claiming, to skip triggers that would not be delivered.
pub fn check_delivery(
    trigger: &Trigger,
    grants: &EffectiveGrants,
    ctx: &InstallationContext,
) -> Result<(), String> {
    let kind = trigger_kind(trigger);
    if !grants.triggers.contains(&kind) {
        return Err(format!(
            "`{}` trigger not delivered: not in `triggers`",
            kind.as_str()
        ));
    }
    if let Some(message) = trigger_message(trigger) {
        if message.author.id == ctx.bot_user_id {
            return Err("the bot's own message is never delivered to it".to_string());
        }
        if !grants.channels.contains_key(&message.channel) {
            return Err(format!(
                "message in `{}` not delivered: not in `read.channels`",
                message.channel
            ));
        }
        let event = event_kind(message);
        if !grants.events.contains(&event) {
            return Err(format!(
                "`{}` message not delivered: not in `read.events`",
                event.as_str()
            ));
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
