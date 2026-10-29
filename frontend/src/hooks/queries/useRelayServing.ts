import { useCallback, useEffect } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke, listen } from "../../bridge";

/** "Be a relay": consent to forward others' encrypted traffic (relay-overlay-design.md §10.2). Not `overlay_mode`; grants no read access (§8). */

/** Mirrors Rust `RelayServingConfig` (snake_case wire format). */
export interface RelayServingConfig {
  /** Master consent. Off by default; never implied by any other setting. */
  enabled: boolean;
  /** Only relay while the device reports an unmetered (Wi-Fi) connection. */
  wifi_only: boolean;
  /** Only relay while the device is on external power. */
  power_only: boolean;
}

/** `waiting` = consented but a condition is unmet (see `hold`). */
export type RelayServingState = "off" | "serving" | "waiting" | "unsupported";

/** Mirrors Rust `RelayServingHold`. */
export type RelayServingHold =
  | "metered_network"
  | "on_battery"
  | "offline"
  | "no_inbound_path";

/** Mirrors Rust `RelayServingStatus`; counters are volume only (§8). */
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

/** Conditions default on (§11.6); consent defaults off. */
export const RELAY_SERVING_DEFAULTS: RelayServingConfig = {
  enabled: false,
  wifi_only: true,
  power_only: true,
};

/** Backend → frontend push whenever the state or a platform signal changes. */
export const RELAY_SERVING_EVENT = "relay-serving-status";

const RELAY_SERVING_KEY = ["relay-serving", "status"] as const;

/** Live status, or `null` when the host can't report it (not an error). Event-driven. */
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

/** Apply `config` live; resolves with the settled status, rejects without a relay surface. */
export async function applyRelayServing(
  config: RelayServingConfig,
): Promise<RelayServingStatus> {
  return invoke<RelayServingStatus>("set_relay_serving", { config });
}

/** `applyRelayServing` plus a cache write of the settled status. */
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
