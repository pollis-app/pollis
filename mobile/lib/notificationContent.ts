// What a push notification shows (#1599). Stored by pollis-core under `notification_content`, where the headless fetch reads it.

export type NotificationContent = "full" | "name" | "none";

export const NOTIFICATION_CONTENT_OPTIONS: readonly NotificationContent[] = [
  "full",
  "name",
  "none",
];

export const DEFAULT_NOTIFICATION_CONTENT: NotificationContent = "full";

/** Unset is `full`; anything unknown fails closed to `none`, never open to `full`. */
export function coerceNotificationContent(raw: unknown): NotificationContent {
  if (raw === null || raw === undefined) {
    return DEFAULT_NOTIFICATION_CONTENT;
  }
  if (raw === "full" || raw === "name") {
    return raw;
  }
  return "none";
}

