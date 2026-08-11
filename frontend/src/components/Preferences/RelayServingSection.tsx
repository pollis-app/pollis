import React from "react";
import { Switch } from "../ui/Switch";
import {
  relayServingHoldLabel,
  type RelayServingConfig,
  type RelayServingStatus,
} from "../../hooks/queries/useRelayServing";
import { formatFileSize } from "../../utils/format";

interface RelayServingSectionProps {
  /** The user's saved choice (synced preferences blob). */
  config: RelayServingConfig;
  /** Live backend status, or null when the host can't report it. */
  status: RelayServingStatus | null;
  /** Last apply error, shown inline. Null when the last apply succeeded. */
  applyError: string | null;
  onChange: (next: RelayServingConfig) => void;
}

/**
 * "Run a relay for others" — the §10.2 consent to let this device carry other
 * people's traffic. Kept in its own section, with its own heading and its own
 * opening sentence, so it can never be read as part of the "Network privacy
 * (relay)" control above it: that one routes YOUR traffic, this one routes
 * OTHER PEOPLE'S traffic through YOUR device, and neither implies the other.
 *
 * Copy here is a review gate (`docs/relay-overlay-design.md` §2.2, §2.3, §3):
 * the honest claim is "hides IP addresses from our servers" — never
 * anonymity, never a social-graph claim, and never presented as the reason
 * Pollis is end-to-end encrypted.
 */
export const RelayServingSection: React.FC<RelayServingSectionProps> = ({
  config,
  status,
  applyError,
  onChange,
}) => {
  return (
    <section className="flex flex-col gap-4 mb-12" data-testid="pref-relay-serving">
      <h2 className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b border-line text-fg">
        Run a relay for others
      </h2>

      <p className="text-xs font-mono text-muted">
        A separate choice from the setting above. That one decides whether{" "}
        <span className="text-dim">your</span> traffic goes through a relay.
        This one decides whether{" "}
        <span className="text-dim">other people&apos;s</span> traffic goes
        through <span className="text-dim">your device</span>. Turning on
        either never turns on the other.
      </p>

      <Switch
        id="pref-relay-serving-enabled"
        data-testid="pref-relay-serving-enabled"
        label="Relay traffic for other Pollis users"
        description="Off unless you switch it on. You can switch it back off at any time."
        checked={config.enabled}
        onChange={(enabled) => onChange({ ...config, enabled })}
      />

      <div className="flex flex-col gap-2 rounded-panel border border-line bg-surface p-3">
        <p className="text-xs font-mono text-dim">What your device would do</p>
        <p className="text-xs font-mono text-muted">
          It forwards other people&apos;s already-encrypted traffic on to
          Pollis&apos;s own servers, so those servers see your address for that
          traffic instead of theirs. Your device only ever forwards to
          Pollis&apos;s servers — never to anyone else&apos;s, and never to the
          open internet.
        </p>
        <p className="text-xs font-mono text-dim">What your device would see</p>
        <p className="text-xs font-mono text-muted">
          Nothing. Relaying grants no read access of any kind. What passes
          through is a sealed stream your device holds no key to: you
          can&apos;t read messages, files or calls, you can&apos;t see who sent
          them or who they&apos;re for, and you can&apos;t tell one kind of
          traffic from another. All your device learns is that some Pollis
          client is exchanging encrypted bytes with a Pollis server. You
          also can&apos;t alter anything in transit — tampering only breaks the
          connection, which the sender routes around.
        </p>
        <p className="text-xs font-mono text-dim">What it costs you</p>
        <p className="text-xs font-mono text-muted">
          Bandwidth, and — without the conditions below — battery and metered
          data.
        </p>
      </div>

      <p className="text-xs font-mono text-muted">
        To be exact about what relaying buys: it hides IP addresses from our
        servers. That is the whole of it. It is not anonymity, it is no
        defence against someone who can watch both ends of a connection, and it
        does not hide who talks to whom — our servers still see that from the
        account making the request, relay or no relay. It is also not what
        makes Pollis end-to-end encrypted: your messages are encrypted on the
        sending device whether or not anyone runs a relay. Relaying is
        metadata protection layered underneath that guarantee, never the
        evidence for it.
      </p>

      <p className="text-xs font-mono uppercase tracking-widest text-dim">
        Conditions
      </p>

      <Switch
        id="pref-relay-serving-wifi-only"
        data-testid="pref-relay-serving-wifi-only"
        label="Only relay on Wi-Fi"
        description="Stops relaying on cellular, or on any connection your device reports as metered. On by default."
        checked={config.wifi_only}
        onChange={(wifi_only) => onChange({ ...config, wifi_only })}
      />

      <Switch
        id="pref-relay-serving-power-only"
        data-testid="pref-relay-serving-power-only"
        label="Only relay while plugged in"
        description="Stops relaying while this device is running on battery. On by default."
        checked={config.power_only}
        onChange={(power_only) => onChange({ ...config, power_only })}
      />

      <p className="text-xs font-mono text-muted" data-testid="pref-relay-serving-status">
        Status: {describeStatus(config, status)}
      </p>

      {status !== null && (
        <p className="text-xs font-mono text-muted">
          This device reports: Wi-Fi {describeSignal(status.on_wifi)}, power{" "}
          {describeSignal(status.on_power)}.
        </p>
      )}

      {applyError !== null && (
        <p
          data-testid="pref-relay-serving-error"
          className="text-xs font-mono text-danger"
        >
          Couldn&apos;t apply this to the running app: {applyError}. Your choice
          is saved and takes effect once the relay service is reachable.
        </p>
      )}
    </section>
  );
};

/** One plain sentence describing what the device is doing right now. */
function describeStatus(
  config: RelayServingConfig,
  status: RelayServingStatus | null,
): string {
  if (status === null) {
    return config.enabled
      ? "this build can't report relay status, so Pollis can't confirm whether this device is relaying."
      : "not relaying.";
  }
  switch (status.state) {
    case "unsupported":
      return "relaying isn't available on this device.";
    case "off":
      return "not relaying.";
    case "waiting":
      return status.hold === null
        ? "not relaying right now."
        : `not relaying — ${relayServingHoldLabel(status.hold)}.`;
    case "serving":
      return `relaying — carrying ${status.active_circuits} ${
        status.active_circuits === 1 ? "connection" : "connections"
      }, ${formatForwarded(status.bytes_forwarded)} forwarded since Pollis started.`;
  }
}

/** `formatFileSize` returns "" for zero, which reads as a missing value here. */
function formatForwarded(bytes: number): string {
  if (bytes === 0) {
    return "0B";
  }
  return formatFileSize(bytes);
}

/** Render a tri-state platform signal without pretending unknown means no. */
function describeSignal(value: boolean | null): string {
  if (value === null) {
    return "unknown";
  }
  return value ? "yes" : "no";
}
