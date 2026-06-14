import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";

// Device-local message retention window, in days. 0 = Forever (no eviction).
// Stored in the local SQLite `ui_state` table (not synced); the backend runs
// an eviction sweep over the local message table whenever this is changed.
export const RETENTION_FOREVER = 0;

// Allowed retention windows, validated identically in the Rust core
// (`set_message_retention` rejects anything not in this set).
export const MESSAGE_RETENTION_OPTIONS = [
  { label: "Forever", days: RETENTION_FOREVER },
  { label: "1 year", days: 365 },
  { label: "90 days", days: 90 },
  { label: "30 days", days: 30 },
] as const;

const messageRetentionKey = ["message_retention"] as const;

// Query: the current device-local retention window in days (0 = Forever).
export function useMessageRetention() {
  return useQuery({
    queryKey: messageRetentionKey,
    queryFn: async (): Promise<number> => {
      return await invoke<number>("get_message_retention");
    },
    staleTime: 1000 * 60 * 5,
  });
}

// Mutation: set the retention window. The backend triggers an immediate
// eviction sweep, so we refetch the value on success to stay in sync.
export function useSetMessageRetention() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (days: number): Promise<void> => {
      await invoke("set_message_retention", { days });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: messageRetentionKey });
    },
  });
}
