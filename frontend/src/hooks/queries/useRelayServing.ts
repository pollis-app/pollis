import { useCallback, useEffect } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke, listen } from "../../bridge";

/**
 * "Be a relay": consent to forward OTHER people's encrypted traffic to Pollis's
 * servers (`docs/relay-overlay-design.md` §10.2, §11.6). Independent of
 * `overlay_mode`, opt-in only. A relay holds no key to what it forwards (§8).
 */

/** Mirrors the Rust `RelayServingConfig` (snake_case wire), as persisted in `relay_serving*`. */
export interface RelayServingConfig {
  /** Master consent; never implied by any other setting. */
  enabled: boolean;
  wifi_only: boolean;
  power_only: boolean;
}

/** `waiting`: consented but a condition is unmet (see `hold`); `unsupported`: can't relay here. */
export type RelayServingState = "off" | "serving" | "waiting" | "unsupported";

/** Mirrors the Rust `RelayServingHold` (snake_case). */
export type RelayServingHold =
  | "metered_network"
  | "on_battery"
  | "offline"
  | "no_inbound_path";

/** Mirrors the Rust `RelayServingStatus`. Counters are local volume only, never who sent it (§8). */
export interface RelayServingStatus {
  config: RelayServingConfig;
  state: RelayServingState;
  hold: RelayServingHold | null;
  /** Null when this platform can't report it. */
  on_wifi: boolean | null;
  /** Null when this platform can't report it. */
  on_power: boolean | null;
  active_circuits: number;
  bytes_forwarded: number;
}

/** Consent OFF; both conditions ON (relaying on metered or battery is user-hostile, §11.6). */
export const RELAY_SERVING_DEFAULTS: RelayServingConfig = {
  enabled: false,
  wifi_only: true,
  power_only: true,
};

export const RELAY_SERVING_EVENT = "relay-serving-status";

const RELAY_SERVING_KEY = ["relay-serving", "status"] as const;

/**
 * Live status, event-driven, or `null` when the host can't report it: a normal
 * state where the UI says it can't confirm rather than claiming success.
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
 * Apply `config` live; resolves with the settled status (consent on battery
 * settles to `waiting`). Rejects without a serving surface; callers persist
 * the choice anyway.
 */
export async function applyRelayServing(
  config: RelayServingConfig,
): Promise<RelayServingStatus> {
  return invoke<RelayServingStatus>("set_relay_serving", { config });
}

/** [`applyRelayServing`] plus writing the settled status to the cache, for instant feedback. */
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
