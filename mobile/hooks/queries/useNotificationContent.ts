// Notification content (#1599): device-local, stored by pollis-core (`notification_content`) so the push extension reads it.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../lib/native";
import {
  coerceNotificationContent,
  DEFAULT_NOTIFICATION_CONTENT,
  type NotificationContent,
} from "../../lib/notificationContent";
import { appStore } from "../../stores/appStore";

export const notificationContentQueryKeys = {
  user: (userId: string | null) => ["notificationContent", userId] as const,
};

export function useNotificationContent() {
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const queryClient = useQueryClient();
  const key = notificationContentQueryKeys.user(userId);

  const query = useQuery({
    queryKey: key,
    queryFn: async (): Promise<NotificationContent> =>
      coerceNotificationContent(await invoke<string>("get_notification_content")),
    enabled: !!userId,
  });

  // Optimistic: the chip moves on the same frame, and rolls back if the write fails.
  const set = useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "toggle" } },
    mutationFn: async (value: NotificationContent) => {
      await invoke("set_notification_content", { mode: value });
      return value;
    },
    onMutate: async (value) => {
      await queryClient.cancelQueries({ queryKey: key });
      const previous = queryClient.getQueryData<NotificationContent>(key);
      queryClient.setQueryData(key, value);
      return { previous };
    },
    onError: (_err, _value, context) => {
      queryClient.setQueryData(key, context?.previous ?? DEFAULT_NOTIFICATION_CONTENT);
    },
  });

  return {
    value: query.data ?? DEFAULT_NOTIFICATION_CONTENT,
    isLoading: query.isPending,
    set,
  };
}
