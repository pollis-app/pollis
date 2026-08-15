import React from "react";
import { Trans, useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { Switch } from "../ui/Switch";
import {
  type RelayServingConfig,
  type RelayServingHold,
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
  const { t } = useTranslation("settings");

  return (
    <section className="flex flex-col gap-4 mb-12" data-testid="pref-relay-serving">
      <h2 className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b border-line text-fg">
        {t("relayServing.heading")}
      </h2>

      <p className="text-xs font-mono text-muted">
        <Trans
          t={t}
          i18nKey="relayServing.intro"
          components={{ dim: <span className="text-dim" /> }}
        />
      </p>

      <Switch
        id="pref-relay-serving-enabled"
        data-testid="pref-relay-serving-enabled"
        label={t("relayServing.enabledLabel")}
        description={t("relayServing.enabledDescription")}
        checked={config.enabled}
        onChange={(enabled) => onChange({ ...config, enabled })}
      />

      <div className="flex flex-col gap-2 rounded-panel border border-line bg-surface p-3">
        <p className="text-xs font-mono text-dim">{t("relayServing.wouldDoHeading")}</p>
        <p className="text-xs font-mono text-muted">{t("relayServing.wouldDoBody")}</p>
        <p className="text-xs font-mono text-dim">{t("relayServing.wouldSeeHeading")}</p>
        <p className="text-xs font-mono text-muted">{t("relayServing.wouldSeeBody")}</p>
        <p className="text-xs font-mono text-dim">{t("relayServing.costHeading")}</p>
        <p className="text-xs font-mono text-muted">{t("relayServing.costBody")}</p>
      </div>

      <p className="text-xs font-mono text-muted">{t("relayServing.exactBody")}</p>

      <p className="text-xs font-mono uppercase tracking-widest text-dim">
        {t("relayServing.conditionsHeading")}
      </p>

      <Switch
        id="pref-relay-serving-wifi-only"
        data-testid="pref-relay-serving-wifi-only"
        label={t("relayServing.wifiOnlyLabel")}
        description={t("relayServing.wifiOnlyDescription")}
        checked={config.wifi_only}
        onChange={(wifi_only) => onChange({ ...config, wifi_only })}
      />

      <Switch
        id="pref-relay-serving-power-only"
        data-testid="pref-relay-serving-power-only"
        label={t("relayServing.powerOnlyLabel")}
        description={t("relayServing.powerOnlyDescription")}
        checked={config.power_only}
        onChange={(power_only) => onChange({ ...config, power_only })}
      />

      <p className="text-xs font-mono text-muted" data-testid="pref-relay-serving-status">
        {t("relayServing.statusLine", { status: describeStatus(t, config, status) })}
      </p>

      {status !== null && (
        <p className="text-xs font-mono text-muted">
          {t("relayServing.signals", {
            wifi: describeSignal(t, status.on_wifi),
            power: describeSignal(t, status.on_power),
          })}
        </p>
      )}

      {applyError !== null && (
        <p
          data-testid="pref-relay-serving-error"
          className="text-xs font-mono text-danger"
        >
          {t("relayServing.applyError", { error: applyError })}
        </p>
      )}
    </section>
  );
};

/** One plain sentence describing what the device is doing right now. */
function describeStatus(
  t: TFunction<"settings">,
  config: RelayServingConfig,
  status: RelayServingStatus | null,
): string {
  if (status === null) {
    return config.enabled
      ? t("relayServing.statusUnreportable")
      : t("relayServing.statusOff");
  }
  switch (status.state) {
    case "unsupported":
      return t("relayServing.statusUnsupported");
    case "off":
      return t("relayServing.statusOff");
    case "waiting":
      return status.hold === null
        ? t("relayServing.statusWaitingUnknown")
        : t("relayServing.statusWaiting", { reason: holdLabel(t, status.hold) });
    case "serving":
      return t("relayServing.statusServing", {
        count: status.active_circuits,
        bytes: formatForwarded(status.bytes_forwarded),
      });
  }
}

/**
 * Plain-language reason a consented device isn't relaying right now.
 *
 * Translated here rather than in `useRelayServing.ts`: that module is not a
 * component, so it has no `t`, and a module-level lookup would freeze the copy
 * at the language active on first import. The `switch` still makes a new Rust
 * variant a compile error at this call site.
 */
function holdLabel(t: TFunction<"settings">, hold: RelayServingHold): string {
  switch (hold) {
    case "metered_network":
      return t("relayServing.holdMeteredNetwork");
    case "on_battery":
      return t("relayServing.holdOnBattery");
    case "offline":
      return t("relayServing.holdOffline");
    case "no_inbound_path":
      return t("relayServing.holdNoInboundPath");
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
function describeSignal(t: TFunction<"settings">, value: boolean | null): string {
  if (value === null) {
    return t("relayServing.signalUnknown");
  }
  return value ? t("relayServing.signalYes") : t("relayServing.signalNo");
}
