import { useCallback, useEffect } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke, listen } from "../../bridge";

/**
 * "Be a relay" — the consent to let THIS device forward other people's
 * encrypted traffic to Pollis's own servers (`docs/relay-overlay-design.md`
 * §10.2, §11.6).
 *
 * This is deliberately NOT the same thing as `overlay_mode` in
 * `usePreferences.ts`. `overlay_mode` decides whether *your* traffic goes
 * through the overlay; this decides whether *other people's* traffic goes
 * through *your* device. Neither implies the other, and this one is opt-in
 * only — see `RELAY_SERVING_DEFAULTS`.
 *
 * What a relay can see is bounded by design (§8): it terminates only the
 * outer relay hop and forwards an opaque stream destined for a first-party
 * host. It holds no key to that stream, so relaying grants the relaying
 * device no read access of any kind.
 */

/**
 * The conditions + consent, exactly as persisted in the synced preferences
 * blob (`relay_serving*` fields) and as sent to the backend.
 *
 * Mirrors the Rust `RelayServingConfig` — wire format is snake_case (this
 * travels inside an object argument, so no camelCase rewrite applies).
 * Keep in sync with the Rust struct.
 */
export interface RelayServingConfig {
  /** Master consent. Off by default; never implied by any other setting. */
  enabled: boolean;
  /** Only relay while the device reports an unmetered (Wi-Fi) connection. */
  wifi_only: boolean;
  /** Only relay while the device is on external power. */
  power_only: boolean;
}

/**
 * What the device is actually doing right now.
 *
 *   - `off`         — no consent given, nothing is being forwarded.
 *   - `serving`     — consented and conditions met; carrying traffic.
 *   - `waiting`     — consented but a condition is unmet (see `hold`).
 *   - `unsupported` — this build/platform cannot serve as a relay at all.
 */
export type RelayServingState = "off" | "serving" | "waiting" | "unsupported";

/**
 * Why a consented device is not currently relaying. Mirrors the Rust
 * `RelayServingHold` enum (serde snake_case).
 */
export type RelayServingHold =
  | "metered_network"
  | "on_battery"
  | "offline"
  | "no_inbound_path";

/**
 * Live relay-serving status. Mirrors the Rust `RelayServingStatus` struct
 * (serde snake_case). Counters are local-only and describe traffic volume,
 * never who sent it — a relay has no way to know that (§8).
 */
export interface RelayServingStatus {
  /** The config the backend is actually running with. */
  config: RelayServingConfig;
  state: RelayServingState;
  /** Set when `state` is `waiting`; null otherwise. */
  hold: RelayServingHold | null;
  /** Live platform signal; null when this platform can't report it. */
  on_wifi: boolean | null;
  /** Live platform signal; null when this platform can't report it. */
  on_power: boolean | null;
  /** Circuits currently being carried. */
  active_circuits: number;
  /** Bytes forwarded since the app started. */
  bytes_forwarded: number;
}

/**
 * Off, Wi-Fi-only, power-only. The two conditions default ON because being a
 * relay on a metered or battery-powered device is user-hostile if
 * mis-defaulted (§11.6). `enabled` defaults OFF because consent to carry
 * other people's traffic is never implied.
 */
export const RELAY_SERVING_DEFAULTS: RelayServingConfig = {
  enabled: false,
  wifi_only: true,
  power_only: true,
};

/** Backend → frontend push whenever the state or a platform signal changes. */
export const RELAY_SERVING_EVENT = "relay-serving-status";

const RELAY_SERVING_KEY = ["relay-serving", "status"] as const;

/**
 * Live relay-serving status, or `null` when the host can't report it (older
 * build, or the command is unavailable). Null is a normal state, not an
 * error: the consent toggle still persists, the UI just says plainly that it
 * cannot confirm what the device is doing rather than claiming success.
 *
 * Updates are event-driven (`RELAY_SERVING_EVENT`) — no polling, per
 * CLAUDE.md.
 */
export function useRelayServingStatus() {
  const queryClient = useQueryClient();

  const query = useQuery({
    queryKey: RELAY_SERVING_KEY,
    queryFn: async (): Promise<RelayServingStatus | null> => {
      try {
        return await invoke<RelayServingStatus>("get_relay_serving_status");
      } catch (e) {
        console.warn("[relay-serving] get_relay_serving_status unavailable", e);
        return null;
      }
    },
    staleTime: 1000 * 30,
  });

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void (async () => {
      try {
        const fn = await listen<RelayServingStatus>(RELAY_SERVING_EVENT, (payload) => {
          queryClient.setQueryData(RELAY_SERVING_KEY, payload);
        });
        if (cancelled) {
          fn();
        } else {
          unlisten = fn;
        }
      } catch (e) {
        console.warn("[relay-serving] status event subscribe failed", e);
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [queryClient]);

  return query;
}

/**
 * Apply `config` to the running relay, live. Resolves with the status the
 * backend settled on (which may differ from the request — e.g. consent given
 * while on battery settles to `waiting`). Rejects when the host has no relay
 * serving surface; callers persist the user's choice regardless and surface
 * the failure inline.
 */
export async function applyRelayServing(
  config: RelayServingConfig,
): Promise<RelayServingStatus> {
  return invoke<RelayServingStatus>("set_relay_serving", { config });
}

/**
 * [`applyRelayServing`] plus a write of the settled status into the query
 * cache, so the status line updates the moment a toggle flips instead of
 * waiting for the backend's next push.
 */
export function useApplyRelayServing() {
  const queryClient = useQueryClient();
  return useCallback(
    async (config: RelayServingConfig): Promise<RelayServingStatus> => {
      const status = await applyRelayServing(config);
      queryClient.setQueryData(RELAY_SERVING_KEY, status);
      return status;
    },
    [queryClient],
  );
}

/** True when the two configs are the same in every field. */
export function relayServingConfigEquals(
  a: RelayServingConfig,
  b: RelayServingConfig,
): boolean {
  return (
    a.enabled === b.enabled &&
    a.wifi_only === b.wifi_only &&
    a.power_only === b.power_only
  );
}

// The plain-language reason a consented device isn't relaying used to live
// here as English literals. It is now `holdLabel` in
// `components/Preferences/RelayServingSection.tsx`, which resolves the same
// exhaustive switch through the translation catalogue (#855) — a hook is the
// wrong place for copy, and `t()` must not run at module scope. The switch is
// still exhaustive over `RelayServingHold`, so a new Rust variant remains a
// compile error, just at the render site instead of here.
