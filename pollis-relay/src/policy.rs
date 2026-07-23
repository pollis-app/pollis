//! Per-target routing policy: overlay vs direct vs degraded-error.
//!
//! This encodes the plane split (design §6.4) and the first-party allowlist as
//! *data*, and it is pure and unit-testable — no sockets. The shim asks
//! [`RoutingPolicy::plan`] what to do with a target, then executes; if an overlay
//! attempt fails, [`PlannedRoute::fallback_to_direct`] decides whether to fall
//! back to a direct dial (Prefer) or surface a degraded error (Strict).

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use crate::server::{Allowlist, HostPattern};

/// The user's overlay mode (design §10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayMode {
    /// Overlay inert: every target routes direct.
    Off,
    /// Control-plane hosts route overlay, falling back to direct on failure.
    Prefer,
    /// Control-plane hosts MUST route overlay; on failure, degrade (never drop,
    /// never silently go direct) — messages-must-work.
    Strict,
}

impl OverlayMode {
    /// Stable `u8` encoding so the mode can live in an [`AtomicU8`] the shim's
    /// routing policy reads live (runtime Prefer↔Strict, design §14 / apply slice).
    pub(crate) fn as_u8(self) -> u8 {
        match self {
            OverlayMode::Off => 0,
            OverlayMode::Prefer => 1,
            OverlayMode::Strict => 2,
        }
    }

    pub(crate) fn from_u8(v: u8) -> OverlayMode {
        match v {
            1 => OverlayMode::Prefer,
            2 => OverlayMode::Strict,
            _ => OverlayMode::Off,
        }
    }
}

/// What the shim should attempt for a given target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannedRoute {
    /// Dial the target directly.
    Direct,
    /// Route through the overlay. If the circuit can't be built, fall back to a
    /// direct dial when `fallback_to_direct`, else surface a degraded error.
    Overlay { fallback_to_direct: bool },
}

/// The final action, after an overlay attempt is known to have succeeded or
/// failed. Pure — used both by the shim and by unit tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalAction {
    Direct,
    Overlay,
    /// Strict mode with no usable circuit: surface an error to the caller.
    Degraded,
}

/// Decides how each target is routed, given the mode, the set of overlay
/// (control-plane) hosts, and the set of always-direct (media) hosts.
///
/// `mode` is held in a shared [`AtomicU8`] rather than a plain field so a running
/// shim can be flipped between `Prefer` and `Strict` **live** — no shim restart,
/// no DB reconnect — via [`OverlayHandle::set_mode`](crate::shim::OverlayHandle::set_mode).
/// The host sets are immutable for the shim's lifetime (a mode change never
/// re-derives the plane split).
#[derive(Clone)]
pub struct RoutingPolicy {
    mode: Arc<AtomicU8>,
    /// Hosts routed through the overlay when the mode allows (control plane).
    overlay_hosts: Vec<HostPattern>,
    /// Hosts always dialed directly regardless of mode (media plane, §6.4).
    direct_hosts: Vec<HostPattern>,
}

impl RoutingPolicy {
    pub fn new(mode: OverlayMode, overlay_hosts: Allowlist, direct_hosts: Allowlist) -> RoutingPolicy {
        RoutingPolicy {
            mode: Arc::new(AtomicU8::new(mode.as_u8())),
            overlay_hosts: overlay_hosts.into_patterns(),
            direct_hosts: direct_hosts.into_patterns(),
        }
    }

    pub fn mode(&self) -> OverlayMode {
        OverlayMode::from_u8(self.mode.load(Ordering::Relaxed))
    }

    /// Flip the live routing mode. The shim's next `plan()` observes it — used to
    /// switch Prefer↔Strict without restarting the shim (design §14 apply slice).
    pub fn set_mode(&self, mode: OverlayMode) {
        self.mode.store(mode.as_u8(), Ordering::Relaxed);
    }

    /// A handle onto the shared mode cell, so the [`OverlayHandle`] can flip it
    /// after the policy has been moved into the shim.
    pub(crate) fn mode_atomic(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.mode)
    }

    fn is_direct_host(&self, host: &str) -> bool {
        self.direct_hosts.iter().any(|p| p.matches(host))
    }

    fn is_overlay_host(&self, host: &str) -> bool {
        self.overlay_hosts.iter().any(|p| p.matches(host))
    }

    /// Decide the intended route for `host`.
    ///
    /// - Media/direct hosts → Direct in every mode.
    /// - `Off` → Direct for everything.
    /// - Control-plane host + `Prefer` → Overlay (direct fallback).
    /// - Control-plane host + `Strict` → Overlay (degrade on failure).
    /// - Any other host → Direct (e.g. non-first-party like Expo push, §14.4).
    pub fn plan(&self, host: &str) -> PlannedRoute {
        // The media plane is always direct — it must never be taxed by the
        // overlay, even in Strict mode (§6.4).
        if self.is_direct_host(host) {
            return PlannedRoute::Direct;
        }
        match self.mode() {
            OverlayMode::Off => PlannedRoute::Direct,
            OverlayMode::Prefer if self.is_overlay_host(host) => {
                PlannedRoute::Overlay { fallback_to_direct: true }
            }
            OverlayMode::Strict if self.is_overlay_host(host) => {
                PlannedRoute::Overlay { fallback_to_direct: false }
            }
            // Not a control-plane host: nothing to hide here, dial direct.
            _ => PlannedRoute::Direct,
        }
    }

    #[cfg(test)]
    pub(crate) fn mode_handle_for_test(&self) -> Arc<AtomicU8> {
        self.mode_atomic()
    }

    /// Pure resolution of a plan given whether an overlay attempt succeeded.
    /// `overlay_ok == None` means no attempt was made (a Direct plan).
    pub fn reconcile(plan: PlannedRoute, overlay_ok: Option<bool>) -> FinalAction {
        match plan {
            PlannedRoute::Direct => FinalAction::Direct,
            PlannedRoute::Overlay { fallback_to_direct } => match overlay_ok {
                Some(true) => FinalAction::Overlay,
                Some(false) if fallback_to_direct => FinalAction::Direct,
                Some(false) => FinalAction::Degraded,
                None => FinalAction::Degraded,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_mode_flips_plan_live() {
        let policy = RoutingPolicy::new(
            OverlayMode::Prefer,
            Allowlist::from_patterns(["control.test"]),
            Allowlist::default(),
        );
        // Prefer: control-plane host routes overlay with direct fallback.
        assert_eq!(
            policy.plan("control.test"),
            PlannedRoute::Overlay { fallback_to_direct: true }
        );
        // Flip to Strict live — same policy object, no rebuild.
        policy.set_mode(OverlayMode::Strict);
        assert_eq!(
            policy.plan("control.test"),
            PlannedRoute::Overlay { fallback_to_direct: false }
        );
        // A clone shares the same cell — this is what the OverlayHandle holds.
        let handle_cell = policy.mode_handle_for_test();
        handle_cell.store(OverlayMode::Off.as_u8(), Ordering::Relaxed);
        assert_eq!(policy.mode(), OverlayMode::Off);
        assert_eq!(policy.plan("control.test"), PlannedRoute::Direct);
    }
}
