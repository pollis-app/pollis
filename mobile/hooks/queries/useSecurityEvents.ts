// Security-event audit trail for the Security screen. Backed by
// `list_security_events` — enrollments, rejections, revocations, identity
// resets, secret-key rotations (#958). Mirrors desktop's SecurityPage list;
// the `device_revoked` event is the last place a revoked device's name is
// readable (the `user_device` row is deleted on revoke), carried as
// `metadata: "name=<device name>"` (#947).

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

// The backend clamps `limit` to 1..=500 and defaults to 100; fetch the
// default and let the screen slice for display.
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
