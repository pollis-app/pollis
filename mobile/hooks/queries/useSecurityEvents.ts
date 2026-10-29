// A `device_revoked` event is the last place a revoked device's name is
// readable, as `metadata: "name=<device name>"` (#947).

import { useQuery } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";

// Mirrors `SecurityEvent` in pollis-core/src/commands/device_enrollment.rs.
export interface SecurityEvent {
  id: string;
  kind: string;
  device_id: string | null;
  created_at: string;
  metadata: string | null;
}

export const securityEventQueryKeys = {
  list: (userId: string | null) => ["securityEvents", userId] as const,
};

// The backend clamps `limit` to 1..=500.
export function useSecurityEvents(limit = 100) {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: securityEventQueryKeys.list(currentUser?.id ?? null),
    queryFn: async (): Promise<SecurityEvent[]> => {
      if (!currentUser) {
        return [];
      }
      return await invoke<SecurityEvent[]>("list_security_events", {
        userId: currentUser.id,
        limit,
      });
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
  });
}
