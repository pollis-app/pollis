//! The relay-serving state machine: consent + live conditions → what this
//! device is actually doing right now.
//!
//! Pure and I/O-free, so every serving rule is a unit test.
//!
//! # Fail closed on unknown signals
//!
//! `None` (platform can't report) must **never** satisfy a condition; guessing would burn a stranger's data.
//!
//! # The one structural invariant
//!
//! `state == Waiting` **iff** `hold.is_some()`, held by [`RelayServingStatus::evaluate`] being the only constructor.
//!
//! Keep in sync with `useRelayServing.ts`; serde **snake_case**.

use serde::{Deserialize, Serialize};

/// Consent plus the two conditions, as persisted in preferences and sent by the UI.
///
/// `#[serde(default)]`: a partial payload degrades to the safe defaults, never `enabled` on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct RelayServingConfig {
    /// Master consent. Off by default; never implied by any other setting.
    pub enabled: bool,
    pub wifi_only: bool,
    pub power_only: bool,
}

impl Default for RelayServingConfig {
    /// Off, both conditions ON (§11.6); consent is never implied.
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
    Off,
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
    /// Nothing can reach this device to hand it circuits.
    NoInboundPath,
}

/// Live platform signals; `None` is "can't report", never "no".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LinkSignals {
    pub on_wifi: Option<bool>,
    pub on_power: Option<bool>,
    /// `None` is **not** offline: only the two conditions are fail-closed.
    pub online: Option<bool>,
}

/// The platform signals plus whether anything can hand this device circuits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ServingSignals {
    pub link: LinkSignals,
    /// True once this device is reachable as a middle hop. Defaults **false**.
    pub inbound_path: bool,
}

/// Live relay-serving status. Mirrors the TS `RelayServingStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RelayServingStatus {
    pub config: RelayServingConfig,
    pub state: RelayServingState,
    /// `Some` iff `state == Waiting`.
    pub hold: Option<RelayServingHold>,
    pub on_wifi: Option<bool>,
    pub on_power: Option<bool>,
    pub active_circuits: u32,
    pub bytes_forwarded: u64,
}

impl RelayServingStatus {
    /// The only constructor; pairs `state`/`hold` via [`evaluate`].
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
/// `supported == false` wins over everything, including consent.
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

/// The first unmet condition, in the order the user can act on them.
pub fn hold_for(
    config: RelayServingConfig,
    signals: ServingSignals,
) -> Option<RelayServingHold> {
    if signals.link.online == Some(false) {
        return Some(RelayServingHold::Offline);
    }
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
        // Still Unsupported, never an actionable hold.
        assert_eq!(
            evaluate(consented(), ServingSignals::default(), false),
            (RelayServingState::Unsupported, None)
        );
    }

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

    /// Consent on battery settles to Waiting{OnBattery}, never Serving.
    #[test]
    fn consent_on_battery_settles_to_waiting() {
        let mut signals = ready();
        signals.link.on_power = Some(false);
        let status = RelayServingStatus::evaluate(consented(), signals, true, 0, 0);
        assert_eq!(status.state, RelayServingState::Waiting);
        assert_eq!(status.hold, Some(RelayServingHold::OnBattery));
        assert!(!status.is_serving());
    }

    /// `hold.is_some()` iff `state == Waiting`, over every combination.
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

        // `hold` must be JSON null, not absent.
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
