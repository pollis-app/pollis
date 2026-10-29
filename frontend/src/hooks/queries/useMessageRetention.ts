import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";

// Device-local retention in days (0 = Forever); changing it sweeps immediately.
export const RETENTION_FOREVER = 0;

// Rust accepts only these; labels come from `retentionLabel` (#855).
export const MESSAGE_RETENTION_OPTIONS = [
  { days: RETENTION_FOREVER },
  { days: 365 },
  { days: 90 },
  { days: 30 },
] as const;

const messageRetentionKey = ["message_retention"] as const;

export function useMessageRetention() {
  return useQuery({
    queryKey: messageRetentionKey,
    queryFn: async (): Promise<number> => {
      return await invoke<number>("get_message_retention");
    },
    staleTime: 1000 * 60 * 5,
  });
}

// Refetch after the immediate sweep.
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
