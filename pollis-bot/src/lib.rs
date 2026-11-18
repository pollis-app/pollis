//! `pollis_bot`: the Pollis bot runtime (#1182). See `.codesight/wiki/bots.md`
//! ("Runtime") and `pollis-bot/README.md`.
//!
//! - [`trigger`]: what starts a job, and the key hosts claim it under.
//! - [`job`]: the seven-step job loop and the per-installation lock.
//! - [`handler`]: handlers (the built-in alerts handler) and [`app_handler`],
//!   a sandboxed App (#2004) run through `pollis-app-host`, loaded only under
//!   a verified admin approval ([`app_approval`], #2005).
//! - [`action`]: actions and the grant check.
//! - [`core`]: the narrow per-installation surface the loop needs from `pollis-core`.
//! - [`runtime`]: many installations, event-driven wake-ups, webhook submission.
//! - [`webhook`] + [`ratelimit`] + [`secrets`]: the alerts webhook listener.
//! - [`config`], [`installations`], [`status`], [`log`]: config, on-disk
//!   installations, `pollis-bot status`, state logging.
//! - [`post`]: `pollis-bot post`, a client of the local webhook.
//! - [`cli`] + [`app`]: the subcommands, over a [`host::BotHost`]; [`core_host`]
//!   is the real host on `pollis-core`.

pub mod action;
pub mod app;
pub mod app_approval;
pub mod app_cli;
pub mod app_handler;
pub mod cli;
pub mod config;
pub mod core;
pub mod core_host;
pub mod handler;
pub mod host;
pub mod installations;
pub mod job;
pub mod log;
pub mod post;
pub mod ratelimit;
pub mod runtime;
pub mod secrets;
pub mod status;
pub mod trigger;
pub mod webhook;
