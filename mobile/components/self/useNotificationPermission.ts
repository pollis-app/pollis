import { useCallback, useState } from "react";
import { useFocusEffect } from "expo-router";
import { getPushPermissionInfo } from "../../lib/push";

export type NotificationPermission = {
  granted: boolean;
  canAskAgain: boolean;
};

/**
 * The OS notification permission, re-read whenever the screen gains focus so
 * returning from system Settings reflects a change. `null` until first read.
 */
export function useNotificationPermission(): {
  info: NotificationPermission | null;
  refresh: () => void;
} {
  const [info, setInfo] = useState<NotificationPermission | null>(null);
  const refresh = useCallback(() => {
    void getPushPermissionInfo()
      .then(setInfo)
      .catch(() => {});
  }, []);
  useFocusEffect(refresh);
  return { info, refresh };
}
