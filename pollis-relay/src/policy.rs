//! Per-target routing policy: overlay vs direct vs degraded-error.
//!
//! This encodes the plane split (design §6.4) and the first-party allowlist as
//! *data*, and it is pure and unit-testable — no sockets. The shim asks
//! [`RoutingPolicy::plan`] what to do with a target, then executes; if an overlay
//! attempt fails, [`PlannedRoute::fallback_to_direct`] decides whether to fall
//! back to a direct dial (Prefer) or surface a degraded error (Strict).

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
#[derive(Clone)]
pub struct RoutingPolicy {
    mode: OverlayMode,
    /// Hosts routed through the overlay when the mode allows (control plane).
    overlay_hosts: Vec<HostPattern>,
    /// Hosts always dialed directly regardless of mode (media plane, §6.4).
    direct_hosts: Vec<HostPattern>,
}

impl RoutingPolicy {
    pub fn new(mode: OverlayMode, overlay_hosts: Allowlist, direct_hosts: Allowlist) -> RoutingPolicy {
        RoutingPolicy {
            mode,
            overlay_hosts: overlay_hosts.into_patterns(),
            direct_hosts: direct_hosts.into_patterns(),
        }
    }

    pub fn mode(&self) -> OverlayMode {
        self.mode
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
        match self.mode {
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
