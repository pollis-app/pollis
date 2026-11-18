//! Developer tooling for Pollis Apps (#2006), behind the `pollis app` CLI:
//!
//! - [`runner`]: run an App's handlers offline against fixtures through the
//!   real App host (`pollis-app-host`), for `pollis app test`;
//! - [`wizer`]: the deterministic pre-initializer ComponentizeJS runs, so the
//!   same source and lockfile build the same component;
//! - [`app_hash`]: the App hash `pollis app build` prints.
//!
//! A library so the runtime can expose the same commands later
//! (`pollis-bot app …`) without a second binary.

pub mod app_hash;
pub mod runner;
pub mod wizer;

pub use runner::*;
