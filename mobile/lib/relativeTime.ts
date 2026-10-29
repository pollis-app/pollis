// Short relative time for Security rows ("just now", "5m ago", "3d ago", then
// a short date). Server timestamps are SQLite `datetime('now')` — UTC with no
// zone — so they are parsed as UTC (`parseServerTimestamp`); `new Date()` would
// read them in the viewer's zone and shift every age by their UTC offset.

import i18n, { activeLocale } from "../i18n";
import { parseServerTimestamp } from "./newMember";

export function formatRelative(iso: string): string {
  const ms = parseServerTimestamp(iso);
  if (ms === null) {
    return iso;
  }
  const d = new Date(ms);
  const diffMs = Math.max(0, Date.now() - ms);
  const sec = Math.floor(diffMs / 1000);
  if (sec < 60) {
    return i18n.t("mobile:self.security.justNow");
  }
  const min = Math.floor(sec / 60);
  if (min < 60) {
    return i18n.t("mobile:self.security.minutesAgo", { count: min });
  }
  const hr = Math.floor(min / 60);
  if (hr < 48) {
    return i18n.t("mobile:self.security.hoursAgo", { count: hr });
  }
  const day = Math.floor(hr / 24);
  if (day < 30) {
    return i18n.t("mobile:self.security.daysAgo", { count: day });
  }
  return d.toLocaleDateString(activeLocale(), {
    month: "short",
    day: "numeric",
  });
}
