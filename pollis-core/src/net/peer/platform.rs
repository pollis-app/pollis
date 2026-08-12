//! OS probes behind the two relay-serving conditions.
//!
//! Both answers are `Option<bool>` and **`None` is a real answer** — the caller
//! ([`super::conditions::hold_for`]) treats it as "condition not satisfied", so a
//! platform we cannot read simply does not relay under that condition rather
//! than guessing. That is why this module never returns a default of
//! convenience.
//!
//! `on_wifi` means **unmetered link**, matching the shipped UI's wording
//! ("unmetered (Wi-Fi) connection"): wired Ethernet counts, cellular does not.
//!
//! # Freshness, without a timer
//!
//! CLAUDE.md bans periodic polling, so nothing here runs on a schedule. The
//! probe is re-run at every event that could act on the answer: a config apply,
//! a status read (which the UI performs on mount and whenever the backend pushes
//! an event), and every time the engine gains or loses a link. A charger
//! unplugged while the app sits idle is therefore noticed at the next of those,
//! not instantly. Subscribing to native change notifications (netlink/udev on
//! Linux, `SCNetworkReachability` + IOKit on macOS) is the event-driven upgrade
//! and needs no timer either — it is a follow-up, not a polling loop.
//!
//! # Platform coverage
//!
//! - **Linux** — pure sysfs/procfs reads, no subprocesses.
//! - **macOS** — two short-lived subprocesses (`pmset`, `route`), each bounded
//!   by a timeout.
//! - **Everything else (incl. Windows)** — `None` for both signals, so a
//!   consenting device with the default conditions sits in `Waiting` and says so
//!   plainly. Honest and safe; a `GetSystemPowerStatus` / WinRT
//!   `NetworkInformation` probe is the follow-up that lifts it.
//!
//! The string parsing is deliberately split out of the platform-gated wrappers
//! and compiled everywhere, so the parsers are unit-tested on every host rather
//! than only on the OS that produces those strings.

use super::conditions::LinkSignals;

/// Probe both platform signals. Never blocks for long: the Linux path is a
/// handful of small file reads, the macOS path two bounded subprocesses.
pub async fn probe() -> LinkSignals {
    #[cfg(target_os = "linux")]
    {
        linux::probe()
    }
    #[cfg(target_os = "macos")]
    {
        macos::probe().await
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        LinkSignals::default()
    }
}

/// How a network interface should count toward "unmetered".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkKind {
    /// Wi-Fi or wired — unmetered.
    Unmetered,
    /// Cellular / mobile broadband — metered.
    Metered,
    /// A tunnel (VPN) or anything we do not recognise. The underlying link
    /// could be anything, so we decline to claim it is unmetered.
    Unknown,
}

/// Classify an interface name. Tunnels are deliberately `Unknown` rather than
/// `Unmetered`: a VPN over cellular looks exactly like a VPN over fibre from
/// here, and claiming the latter would burn a stranger's data plan.
pub(crate) fn classify_iface(name: &str) -> LinkKind {
    let n = name.to_ascii_lowercase();
    let cellular = ["wwan", "wwp", "rmnet", "ppp", "qmi", "pdp_ip", "ccmni"];
    if cellular.iter().any(|p| n.starts_with(p)) {
        return LinkKind::Metered;
    }
    let tunnel = ["tun", "tap", "wg", "utun", "ipsec", "ppp0", "gpd"];
    if tunnel.iter().any(|p| n.starts_with(p)) {
        return LinkKind::Unknown;
    }
    // en*/eth*/wl*/br*/bond*/usb* — wired, Wi-Fi, or a bridge over them.
    LinkKind::Unmetered
}

/// Turn a classification into the `on_wifi` signal.
pub(crate) fn kind_to_signal(kind: LinkKind) -> Option<bool> {
    match kind {
        LinkKind::Unmetered => Some(true),
        LinkKind::Metered => Some(false),
        LinkKind::Unknown => None,
    }
}

/// Parse the interface out of `route -n get default` (macOS).
///
/// Compiled everywhere so its tests run everywhere; only macOS calls it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn parse_route_get_default(output: &str) -> Option<String> {
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("interface:") {
            let iface = rest.trim();
            if !iface.is_empty() {
                return Some(iface.to_string());
            }
        }
    }
    None
}

/// Parse `pmset -g batt` (macOS): "Now drawing from 'AC Power'".
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn parse_pmset_power(output: &str) -> Option<bool> {
    let lower = output.to_ascii_lowercase();
    if lower.contains("'ac power'") || lower.contains("\"ac power\"") {
        return Some(true);
    }
    if lower.contains("'battery power'") || lower.contains("\"battery power\"") {
        return Some(false);
    }
    None
}

/// Parse the default-route interface out of `/proc/net/route` (Linux). The
/// default route is the row whose destination is all zeroes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_proc_net_route(contents: &str) -> Option<String> {
    for line in contents.lines().skip(1) {
        let mut cols = line.split_whitespace();
        let iface = cols.next()?;
        let dest = cols.next()?;
        if dest.eq_ignore_ascii_case("00000000") {
            return Some(iface.to_string());
        }
    }
    None
}

/// Parse the default-route interface out of `/proc/net/ipv6_route`. Columns are
/// `dest prefixlen src srcprefixlen nexthop metric refcnt use flags iface`; the
/// default route is the all-zero destination with prefix length 0.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_proc_net_ipv6_route(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 10 {
            continue;
        }
        let is_default = cols[0].chars().all(|c| c == '0') && cols[1] == "00";
        if is_default {
            let iface = cols[cols.len() - 1];
            if iface != "lo" {
                return Some(iface.to_string());
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::path::Path;

    pub(super) fn probe() -> LinkSignals {
        let iface = default_route_iface();
        let online = match std::fs::metadata("/proc/net/route") {
            Ok(_) => Some(iface.is_some()),
            // No procfs (a sandbox) — we genuinely do not know.
            Err(_) => None,
        };
        let on_wifi = iface.as_deref().and_then(|i| kind_to_signal(classify(i)));
        LinkSignals {
            on_wifi,
            on_power: power(),
            online,
        }
    }

    fn default_route_iface() -> Option<String> {
        if let Ok(v4) = std::fs::read_to_string("/proc/net/route") {
            if let Some(iface) = parse_proc_net_route(&v4) {
                return Some(iface);
            }
        }
        let v6 = std::fs::read_to_string("/proc/net/ipv6_route").ok()?;
        parse_proc_net_ipv6_route(&v6)
    }

    /// Interface names alone do not distinguish `enp3s0` (wired) from a
    /// predictable-named Wi-Fi device on every distro, but sysfs does: a
    /// wireless device has a `wireless/` directory. Both are unmetered, so this
    /// only matters for the `Unknown` fallback.
    fn classify(iface: &str) -> LinkKind {
        if Path::new(&format!("/sys/class/net/{iface}/wireless")).exists() {
            return LinkKind::Unmetered;
        }
        classify_iface(iface)
    }

    /// External power from `/sys/class/power_supply`.
    ///
    /// A machine with **no battery at all** cannot be running on battery — it is
    /// plugged in, or it would not be running — so a desktop with no power-supply
    /// entries answers `Some(true)` rather than blocking every desktop volunteer
    /// behind a condition it can never satisfy.
    fn power() -> Option<bool> {
        let entries = std::fs::read_dir("/sys/class/power_supply").ok()?;
        let mut saw_battery = false;
        let mut mains_online = false;
        for entry in entries.flatten() {
            let path = entry.path();
            let kind = std::fs::read_to_string(path.join("type")).unwrap_or_default();
            let kind = kind.trim().to_ascii_lowercase();
            if kind == "battery" {
                saw_battery = true;
                continue;
            }
            // Mains / USB / USB_PD / Wireless are all external supplies.
            let online = std::fs::read_to_string(path.join("online")).unwrap_or_default();
            if online.trim() == "1" {
                mains_online = true;
            }
        }
        if !saw_battery {
            return Some(true);
        }
        Some(mains_online)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::time::Duration;

    /// Bound each probe: these commands answer in milliseconds, and a wedged one
    /// must not stall a status read.
    const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

    pub(super) async fn probe() -> LinkSignals {
        let iface = run("route", &["-n", "get", "default"])
            .await
            .and_then(|out| parse_route_get_default(&out));
        let on_wifi = iface
            .as_deref()
            .and_then(|i| kind_to_signal(classify_iface(i)));
        let on_power = run("pmset", &["-g", "batt"])
            .await
            .and_then(|out| parse_pmset_power(&out));
        LinkSignals {
            on_wifi,
            on_power,
            online: Some(iface.is_some()),
        }
    }

    async fn run(program: &str, args: &[&str]) -> Option<String> {
        let output = tokio::time::timeout(
            PROBE_TIMEOUT,
            tokio::process::Command::new(program).args(args).output(),
        )
        .await
        .ok()?
        .ok()?;
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cellular_interfaces_are_metered() {
        for name in ["wwan0", "rmnet_data0", "ppp0", "qmi0", "pdp_ip0"] {
            assert_eq!(classify_iface(name), LinkKind::Metered, "{name}");
            assert_eq!(kind_to_signal(classify_iface(name)), Some(false), "{name}");
        }
    }

    #[test]
    fn wired_and_wifi_are_unmetered() {
        for name in ["eth0", "en0", "enp3s0", "wlan0", "wlp2s0", "br0"] {
            assert_eq!(classify_iface(name), LinkKind::Unmetered, "{name}");
            assert_eq!(kind_to_signal(classify_iface(name)), Some(true), "{name}");
        }
    }

    /// A tunnel is unknown, never "unmetered" — the fail-closed choice.
    #[test]
    fn tunnels_are_unknown_not_unmetered() {
        for name in ["tun0", "wg0", "utun3", "ipsec0"] {
            assert_eq!(classify_iface(name), LinkKind::Unknown, "{name}");
            assert_eq!(kind_to_signal(classify_iface(name)), None, "{name}");
        }
    }

    #[test]
    fn parses_proc_net_route_default() {
        let contents = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n\
             wlan0\t00E0A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0\n\
             wlan0\t00000000\t01E0A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n";
        assert_eq!(parse_proc_net_route(contents).as_deref(), Some("wlan0"));

        // No default route at all → offline.
        let no_default = "Iface\tDestination\tGateway\n\
             wlan0\t00E0A8C0\t00000000\n";
        assert_eq!(parse_proc_net_route(no_default), None);
    }

    #[test]
    fn parses_ipv6_default_route() {
        let contents = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 \
             fe800000000000000000000000000001 00000400 00000001 00000000 00000003 eth0\n";
        assert_eq!(parse_proc_net_ipv6_route(contents).as_deref(), Some("eth0"));
    }

    #[test]
    fn parses_pmset() {
        assert_eq!(
            parse_pmset_power("Now drawing from 'AC Power'\n -InternalBattery-0 100%"),
            Some(true)
        );
        assert_eq!(
            parse_pmset_power("Now drawing from 'Battery Power'\n -InternalBattery-0 62%"),
            Some(false)
        );
        assert_eq!(parse_pmset_power("something else entirely"), None);
    }

    #[test]
    fn parses_route_get_default() {
        let output = "   route to: default\ndestination: default\n       gateway: 192.168.1.1\n  interface: en0\n";
        assert_eq!(parse_route_get_default(output).as_deref(), Some("en0"));
        assert_eq!(parse_route_get_default("route: writing to routing socket: not in table"), None);
    }

    /// The probe must never panic or hang on the host running the tests, and
    /// whatever it says must be a value the state machine can consume.
    #[tokio::test]
    async fn probe_answers_without_panicking() {
        let signals = probe().await;
        // Nothing to assert about the values (they depend on the host), but the
        // tri-state contract is that any of them may be None.
        let _ = (signals.on_wifi, signals.on_power, signals.online);
    }
}
