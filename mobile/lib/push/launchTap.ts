// The notification tap that cold-started the app (#1422, #1614). Import-free for node tests.

/** The slice of a notification response this reads; expo-notifications' `NotificationResponse` fits. */
export interface LaunchResponse {
  notification: { request: { content: { data?: unknown } } };
}

/** expo-notifications' last-response store: `getLastNotificationResponse` / `clearLastNotificationResponse`. */
export interface LaunchResponseStore {
  getLast(): LaunchResponse | null;
  clear(): void;
}

/** The launch tap's data, taken once and cleared so it never routes twice; null when there is none. */
export function takeLaunchTap(store: LaunchResponseStore): unknown | null {
  try {
    const response = store.getLast();
    if (!response) {
      return null;
    }
    store.clear();
    return response.notification.request.content.data ?? null;
  } catch {
    return null;
  }
}
