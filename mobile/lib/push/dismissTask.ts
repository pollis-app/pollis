// The background task that clears read notifications (#1508). Defined at module load (mobile/index.ts imports it first); imports no bridge, store or network.

import * as Notifications from "expo-notifications";
import * as TaskManager from "expo-task-manager";
import { pickDismissals, readDismissPayload, type Presented } from "./dismiss";

export const DISMISS_TASK = "pollis-dismiss-read-notifications";

export async function dismissHandles(handles: readonly string[]): Promise<number> {
  if (handles.length === 0) {
    return 0;
  }
  const presented = await Notifications.getPresentedNotificationsAsync();
  const tray: Presented[] = presented.map((n) => ({
    identifier: n.request.identifier,
    data: n.request.content.data,
  }));
  const ids = pickDismissals(tray, handles);
  await Promise.all(
    ids.map((id) =>
      Notifications.dismissNotificationAsync(id).catch(() => {
        // Already gone.
      }),
    ),
  );
  return ids.length;
}

try {
  TaskManager.defineTask<Notifications.NotificationTaskPayload>(
    DISMISS_TASK,
    async ({ data, error }) => {
      if (error) {
        return Notifications.BackgroundNotificationTaskResult.Failed;
      }
      // Notification-action taps carry no `d` and are ignored.
      const handles = readDismissPayload(data);
      if (handles === null) {
        return Notifications.BackgroundNotificationTaskResult.NoData;
      }
      try {
        await dismissHandles(handles);
        return Notifications.BackgroundNotificationTaskResult.NewData;
      } catch {
        return Notifications.BackgroundNotificationTaskResult.Failed;
      }
    },
  );
  void Notifications.registerTaskAsync(DISMISS_TASK).catch(() => {
    // No native task support: the other two paths still clear the tray.
  });
} catch {
  // As above.
}
