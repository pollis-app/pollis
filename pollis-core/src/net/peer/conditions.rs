//! The relay-serving state machine: consent + live conditions → what this
//! device is actually doing right now.
//!
//! Pure and I/O-free on purpose. Every rule that decides whether a volunteer's
//! device carries other people's traffic is a branch in [`evaluate`], so each one
//! is a unit test rather than something you have to unplug a laptop to check.
//!
//! # Fail closed on unknown signals
//!
//! `on_wifi` / `on_power` are `Option<bool>` and `None` means *the platform
//! cannot report it*, which is a first-class answer — the UI renders it as
//! "unknown" rather than "no". It must **never** satisfy a condition: if the
//! user asked to relay only on Wi-Fi and we cannot tell whether we are on Wi-Fi,
//! we do not relay. The alternative — treating "don't know" as "sure, go ahead"
//! — burns a stranger's cellular data on someone else's traffic, which is
//! exactly the promise the consent screen makes.
//!
//! # The one structural invariant
//!
//! `state == Waiting` **iff** `hold.is_some()`. The wire format has to carry the
//! two separately (the TS type in `frontend/src/hooks/queries/useRelayServing.ts`
//! is `state` + a nullable `hold`), so the pairing is held by making
//! [`RelayServingStatus::evaluate`] the only way this crate builds one, plus a
//! test that walks every combination.
//!
//! Keep these types in sync with `useRelayServing.ts` (CLAUDE.md) — serde
//! **snake_case**, matching `ApmConfig`'s convention rather than
//! `MediaPermissions`' camelCase.

use serde::{Deserialize, Serialize};

/// Consent plus the two conditions, exactly as persisted in the synced
/// preferences blob (`relay_serving`, `relay_serving_wifi_only`,
/// `relay_serving_power_only`) and as sent by the UI.
///
/// `#[serde(default)]`: a payload from an older/partial caller degrades field by
/// field to the safe defaults below rather than failing or, worse, defaulting
/// `enabled` to on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct RelayServingConfig {
    /// Master consent. Off by default; never implied by any other setting.
    pub enabled: bool,
    /// Only relay while the device reports an unmetered link.
    pub wifi_only: bool,
    /// Only relay while the device is on external power.
    pub power_only: bool,
}

impl Default for RelayServingConfig {
    /// Off, and both conditions ON. Relaying on a metered or battery-powered
    /// device is user-hostile if mis-defaulted (§11.6); consent to carry other
    /// people's traffic is never implied.
    fn default() -> Self {
        RelayServingConfig {
            enabled: false,
            wifi_only: true,
            power_only: true,
        }
    }
}

/// What the device is actually doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayServingState {
    /// No consent given; nothing is forwarded.
    Off,
    /// Consented, conditions met, carrying traffic.
    Serving,
    /// Consented but a condition is unmet — see the paired hold.
    Waiting,
    /// This build/platform cannot serve as a relay at all.
    Unsupported,
}

/// Why a consented device is not currently relaying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayServingHold {
    /// `wifi_only` is set and the link is metered — or we cannot tell.
    MeteredNetwork,
    /// `power_only` is set and we are on battery — or we cannot tell.
    OnBattery,
    /// No network at all.
    Offline,
    /// Nothing can reach this device to hand it circuits. In v1 that means no
    /// first-party relay has accepted this device as a middle hop.
    NoInboundPath,
}

/// The live platform signals behind the two conditions. `None` is "this platform
/// cannot report it", never "no".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LinkSignals {
    /// Unmetered link (Wi-Fi or wired). `None` when unknown.
    pub on_wifi: Option<bool>,
    /// External power. `None` when unknown.
    pub on_power: Option<bool>,
    /// Whether the device has any network at all. `None` when unknown, which is
    /// deliberately **not** treated as offline: `Offline` is a fact we report
    /// when we know it, and manufacturing it from ignorance would show the user
    /// a hold they cannot act on. The two *conditions* are the fail-closed ones.
    pub online: Option<bool>,
}

/// Everything [`evaluate`] needs: the platform signals plus whether anything can
/// currently hand this device circuits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ServingSignals {
    pub link: LinkSignals,
    /// True once a [`crate::net::peer::PeerLink`] producer is attached and this
    /// device is reachable as a middle hop. Defaults **false** — a device that
    /// cannot be reached is not serving, whatever else is true.
    pub inbound_path: bool,
}

/// Live relay-serving status. Mirrors the TS `RelayServingStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RelayServingStatus {
    /// The config the backend is actually running with.
    pub config: RelayServingConfig,
    pub state: RelayServingState,
    /// `Some` iff `state == Waiting`.
    pub hold: Option<RelayServingHold>,
    pub on_wifi: Option<bool>,
    pub on_power: Option<bool>,
    /// Circuits currently being carried.
    pub active_circuits: u32,
    /// Bytes forwarded since the app started.
    pub bytes_forwarded: u64,
}

impl RelayServingStatus {
    /// The only constructor. Pairs `state`/`hold` through [`evaluate`] so the
    /// two cannot drift apart.
    pub fn evaluate(
        config: RelayServingConfig,
        signals: ServingSignals,
        supported: bool,
        active_circuits: u32,
        bytes_forwarded: u64,
    ) -> RelayServingStatus {
        let (state, hold) = evaluate(config, signals, supported);
        RelayServingStatus {
            config,
            state,
            hold,
            on_wifi: signals.link.on_wifi,
            on_power: signals.link.on_power,
            active_circuits,
            bytes_forwarded,
        }
    }

    /// True when this device should currently be carrying traffic.
    pub fn is_serving(&self) -> bool {
        self.state == RelayServingState::Serving
    }
}

/// Consent + conditions → `(state, hold)`.
///
/// `supported` is false where the platform cannot serve at all; it wins over
/// everything, including consent, because the honest answer there is "relaying
/// isn't available on this device", not "waiting for Wi-Fi".
pub fn evaluate(
    config: RelayServingConfig,
    signals: ServingSignals,
    supported: bool,
) -> (RelayServingState, Option<RelayServingHold>) {
    if !supported {
        return (RelayServingState::Unsupported, None);
    }
    if !config.enabled {
        return (RelayServingState::Off, None);
    }
    match hold_for(config, signals) {
        Some(hold) => (RelayServingState::Waiting, Some(hold)),
        None => (RelayServingState::Serving, None),
    }
}

/// The first unmet condition, in the order the user can act on them: no network
/// beats a metered network, which beats battery, which beats "nothing can reach
/// us" (the only one the user cannot fix by walking to a plug).
pub fn hold_for(
    config: RelayServingConfig,
    signals: ServingSignals,
) -> Option<RelayServingHold> {
    if signals.link.online == Some(false) {
        return Some(RelayServingHold::Offline);
    }
    // Fail closed: `!= Some(true)` covers both "metered" and "can't tell".
    if config.wifi_only && signals.link.on_wifi != Some(true) {
        return Some(RelayServingHold::MeteredNetwork);
    }
    if config.power_only && signals.link.on_power != Some(true) {
        return Some(RelayServingHold::OnBattery);
    }
    if !signals.inbound_path {
        return Some(RelayServingHold::NoInboundPath);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Consented, everything green.
    fn ready() -> ServingSignals {
        ServingSignals {
            link: LinkSignals {
                on_wifi: Some(true),
                on_power: Some(true),
                online: Some(true),
            },
            inbound_path: true,
        }
    }

    fn consented() -> RelayServingConfig {
        RelayServingConfig {
            enabled: true,
            wifi_only: true,
            power_only: true,
        }
    }

    #[test]
    fn defaults_are_off_with_both_conditions_on() {
        let d = RelayServingConfig::default();
        assert!(!d.enabled);
        assert!(d.wifi_only);
        assert!(d.power_only);
    }

    #[test]
    fn consent_plus_conditions_met_serves() {
        assert_eq!(
            evaluate(consented(), ready(), true),
            (RelayServingState::Serving, None)
        );
    }

    #[test]
    fn no_consent_is_off_even_when_everything_else_is_green() {
        let config = RelayServingConfig {
            enabled: false,
            ..consented()
        };
        assert_eq!(
            evaluate(config, ready(), true),
            (RelayServingState::Off, None)
        );
    }

    #[test]
    fn unsupported_wins_over_consent_and_conditions() {
        assert_eq!(
            evaluate(consented(), ready(), false),
            (RelayServingState::Unsupported, None)
        );
        // Even with nothing else satisfied, the answer is still Unsupported —
        // never a hold the user could act on.
        assert_eq!(
            evaluate(consented(), ServingSignals::default(), false),
            (RelayServingState::Unsupported, None)
        );
    }

    /// The load-bearing rule: unknown is not satisfied.
    #[test]
    fn unknown_wifi_does_not_satisfy_wifi_only() {
        let mut signals = ready();
        signals.link.on_wifi = None;
        assert_eq!(
            evaluate(consented(), signals, true),
            (
                RelayServingState::Waiting,
                Some(RelayServingHold::MeteredNetwork)
            )
        );
    }

    #[test]
    fn unknown_power_does_not_satisfy_power_only() {
        let mut signals = ready();
        signals.link.on_power = None;
        assert_eq!(
            evaluate(consented(), signals, true),
            (RelayServingState::Waiting, Some(RelayServingHold::OnBattery))
        );
    }

    #[test]
    fn unknown_signals_are_fine_when_the_user_did_not_ask_for_the_condition() {
        let config = RelayServingConfig {
            enabled: true,
            wifi_only: false,
            power_only: false,
        };
        let signals = ServingSignals {
            link: LinkSignals {
                on_wifi: None,
                on_power: None,
                online: None,
            },
            inbound_path: true,
        };
        assert_eq!(
            evaluate(config, signals, true),
            (RelayServingState::Serving, None)
        );
    }

    #[test]
    fn metered_and_battery_hold_separately() {
        let mut signals = ready();
        signals.link.on_wifi = Some(false);
        assert_eq!(
            hold_for(consented(), signals),
            Some(RelayServingHold::MeteredNetwork)
        );

        let mut signals = ready();
        signals.link.on_power = Some(false);
        assert_eq!(
            hold_for(consented(), signals),
            Some(RelayServingHold::OnBattery)
        );
    }

    #[test]
    fn offline_beats_the_conditions() {
        let mut signals = ready();
        signals.link.online = Some(false);
        signals.link.on_wifi = Some(false);
        signals.link.on_power = Some(false);
        assert_eq!(
            hold_for(consented(), signals),
            Some(RelayServingHold::Offline)
        );
    }

    #[test]
    fn unknown_online_is_not_offline() {
        let mut signals = ready();
        signals.link.online = None;
        assert_eq!(hold_for(consented(), signals), None);
    }

    #[test]
    fn no_reachability_holds_even_with_every_condition_met() {
        let mut signals = ready();
        signals.inbound_path = false;
        assert_eq!(
            evaluate(consented(), signals, true),
            (
                RelayServingState::Waiting,
                Some(RelayServingHold::NoInboundPath)
            )
        );
    }

    /// Consent given while on battery settles to Waiting{OnBattery}, never
    /// Serving — the exact case D2 called out.
    #[test]
    fn consent_on_battery_settles_to_waiting() {
        let mut signals = ready();
        signals.link.on_power = Some(false);
        let status = RelayServingStatus::evaluate(consented(), signals, true, 0, 0);
        assert_eq!(status.state, RelayServingState::Waiting);
        assert_eq!(status.hold, Some(RelayServingHold::OnBattery));
        assert!(!status.is_serving());
    }

    /// `hold.is_some()` iff `state == Waiting`, over every combination of the
    /// three booleans and the four tri-state signals.
    #[test]
    fn hold_is_set_exactly_when_waiting() {
        let tri = [None, Some(true), Some(false)];
        for enabled in [false, true] {
            for wifi_only in [false, true] {
                for power_only in [false, true] {
                    for on_wifi in tri {
                        for on_power in tri {
                            for online in tri {
                                for inbound_path in [false, true] {
                                    for supported in [false, true] {
                                        let config = RelayServingConfig {
                                            enabled,
                                            wifi_only,
                                            power_only,
                                        };
                                        let signals = ServingSignals {
                                            link: LinkSignals {
                                                on_wifi,
                                                on_power,
                                                online,
                                            },
                                            inbound_path,
                                        };
                                        let status = RelayServingStatus::evaluate(
                                            config, signals, supported, 0, 0,
                                        );
                                        assert_eq!(
                                            status.hold.is_some(),
                                            status.state == RelayServingState::Waiting,
                                            "state {:?} / hold {:?}",
                                            status.state,
                                            status.hold
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// The wire contract the shipped UI typechecks against.
    #[test]
    fn serializes_snake_case_to_the_shipped_ts_shape() {
        let mut signals = ready();
        signals.link.on_power = Some(false);
        let status = RelayServingStatus::evaluate(consented(), signals, true, 2, 4096);
        let json = serde_json::to_value(status).unwrap();
        assert_eq!(json["state"], "waiting");
        assert_eq!(json["hold"], "on_battery");
        assert_eq!(json["on_wifi"], true);
        assert_eq!(json["on_power"], false);
        assert_eq!(json["active_circuits"], 2);
        assert_eq!(json["bytes_forwarded"], 4096);
        assert_eq!(json["config"]["wifi_only"], true);
        assert_eq!(json["config"]["power_only"], true);
        assert_eq!(json["config"]["enabled"], true);

        // `hold` must be JSON null (not absent) — the TS type is `| null`.
        assert!(json.get("hold").is_some());
        let off = RelayServingStatus::evaluate(
            RelayServingConfig::default(),
            ServingSignals::default(),
            true,
            0,
            0,
        );
        let json = serde_json::to_value(off).unwrap();
        assert_eq!(json["state"], "off");
        assert!(json["hold"].is_null());
    }

    #[test]
    fn config_deserializes_from_the_ui_payload_and_degrades_safely() {
        let full: RelayServingConfig = serde_json::from_str(
            r#"{"enabled":true,"wifi_only":false,"power_only":true}"#,
        )
        .unwrap();
        assert!(full.enabled);
        assert!(!full.wifi_only);
        assert!(full.power_only);

        // A partial payload takes the safe defaults for what it omits.
        let partial: RelayServingConfig = serde_json::from_str(r#"{"wifi_only":false}"#).unwrap();
        assert!(!partial.enabled);
        assert!(!partial.wifi_only);
        assert!(partial.power_only);
    }

    #[test]
    fn state_and_hold_names_match_the_ts_union() {
        for (state, name) in [
            (RelayServingState::Off, "off"),
            (RelayServingState::Serving, "serving"),
            (RelayServingState::Waiting, "waiting"),
            (RelayServingState::Unsupported, "unsupported"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), name);
        }
        for (hold, name) in [
            (RelayServingHold::MeteredNetwork, "metered_network"),
            (RelayServingHold::OnBattery, "on_battery"),
            (RelayServingHold::Offline, "offline"),
            (RelayServingHold::NoInboundPath, "no_inbound_path"),
        ] {
            assert_eq!(serde_json::to_value(hold).unwrap(), name);
        }
    }
}
