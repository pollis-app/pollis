// Timestamp for a conversation row (Messages.dc.html): a clock time today,
// the weekday within the last week, then a short date. Always formatted in the
// active app locale, never the host's.

import { activeLocale } from "../../i18n";

const DAY_MS = 24 * 60 * 60 * 1000;

export function conversationTime(input: number | string | Date): string {
  const d = input instanceof Date ? input : new Date(input);
  const ts = d.getTime();
  if (Number.isNaN(ts)) {
    return "";
  }
  const now = new Date();
  const locale = activeLocale();
  if (d.toDateString() === now.toDateString()) {
    return d.toLocaleTimeString(locale, { hour: "numeric", minute: "2-digit" });
  }
  if (now.getTime() - ts < 6 * DAY_MS) {
    return d.toLocaleDateString(locale, { weekday: "short" });
  }
  return d.toLocaleDateString(locale, { month: "short", day: "numeric" });
}
