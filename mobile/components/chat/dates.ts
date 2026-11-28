// Day/time formatting for the chat timeline, sentence case.

import { AppState } from "react-native";
import i18n, { activeLocale } from "../../i18n";

export { GROUP_WINDOW_MS } from "../../lib/chatItems";

// One formatter per (locale, options) (#2168): `toLocale*String(locale, options)` builds a fresh
// `Intl.DateTimeFormat` on every call, and every row formats its time on every render. The
// output is the same: each option set names the fields it shows, so `toLocale*` adds no
// defaults. A formatter keeps the time zone it was made in, so the cache is dropped on a
// language change and whenever the app comes back to the foreground (where a zone change shows).
const formats = new Map<string, Intl.DateTimeFormat>();

function format(name: string, options: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
  const locale = activeLocale();
  const key = `${locale}|${name}`;
  let f = formats.get(key);
  if (!f) {
    f = new Intl.DateTimeFormat(locale, options);
    formats.set(key, f);
  }
  return f;
}

i18n.on("languageChanged", () => formats.clear());
AppState.addEventListener("change", (state) => {
  if (state === "active") {
    formats.clear();
  }
});

export function dayKey(ts: number): string {
  return new Date(ts).toDateString();
}

export function dayLabel(ts: number): string {
  const d = new Date(ts);
  const today = new Date();
  if (d.toDateString() === today.toDateString()) {
    return i18n.t("common:time.today");
  }
  const yest = new Date(today);
  yest.setDate(today.getDate() - 1);
  if (d.toDateString() === yest.toDateString()) {
    return i18n.t("common:time.yesterday");
  }
  return format("dayMonth", { month: "short", day: "numeric" }).format(d);
}

export function timeLabel(ts: number): string {
  return format("time", { hour: "2-digit", minute: "2-digit" }).format(ts);
}
