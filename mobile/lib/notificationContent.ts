// What a push notification shows (#1599). Stored by pollis-core in the device's local DB (`notification_content`), where the headless fetch reads it.

export type NotificationContent = "full" | "name" | "none";

export const NOTIFICATION_CONTENT_OPTIONS: readonly NotificationContent[] = [
  "full",
  "name",
  "none",
];

export const DEFAULT_NOTIFICATION_CONTENT: NotificationContent = "full";

/** Mirrors the core's rule: unset is `full`, anything unknown fails closed to `none`. */
export function coerceNotificationContent(raw: unknown): NotificationContent {
  if (raw === null || raw === undefined) {
    return DEFAULT_NOTIFICATION_CONTENT;
  }
  if (raw === "full" || raw === "name") {
    return raw;
  }
  return "none";
}

