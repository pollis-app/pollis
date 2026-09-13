/*
 * Escaping for text that ends up in an OS notification (#1095).
 *
 * A notification body is not plain text on Linux: the freedesktop spec allows a
 * small HTML subset, and `notify-rust` passes the body through verbatim, so a
 * daemon that renders body markup (dunst, KDE) will act on it.
 *
 * Everything interpolated into a notification is remote-controlled — a sender's
 * username, a group name, an inviter's preferred name. So `<img
 * src="http://attacker/x">` in a group name turns message receipt into a request
 * from the victim's machine (an IP-revealing pixel that also bypasses the
 * overlay), and `<a href>` becomes clickable markup in the banner.
 *
 * Pure and dependency-free so it can be unit-tested without the Tauri bridge;
 * `bridge/notifications.ts` applies it at the single point where a notification
 * leaves the renderer, which is what makes it true for every caller — including
 * the next one added, which will not remember to escape at its own i18n
 * interpolation site.
 */

/** Escape the three characters the notification body treats as markup. */
export function escapeNotificationText(text: string): string {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}
