/* OS notification escaping (#1095): Linux daemons render body markup, and the text is remote-controlled. Applied in `bridge/notifications.ts`. */

/** Escape the three characters the notification body treats as markup. */
export function escapeNotificationText(text: string): string {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}
