/*
 * Escaping for OS notification text (#1095). Linux daemons (dunst, KDE) render
 * a body HTML subset, and every interpolated value is remote-controlled, so
 * `<img src>` in a group name becomes an IP-revealing request. Applied once in
 * `bridge/notifications.ts`, so every caller is covered.
 */

/** Escape the three characters the notification body treats as markup. */
export function escapeNotificationText(text: string): string {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}
