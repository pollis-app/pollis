// Day/time formatting helpers for the chat timeline. Labels are sentence
// case ("Today", "Mar 4") — the redesign drops tracked capitals.

import i18n, { activeLocale } from "../../i18n";

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
  return d.toLocaleDateString(activeLocale(), { month: "short", day: "numeric" });
}

export function timeLabel(ts: number): string {
  return new Date(ts).toLocaleTimeString(activeLocale(), {
    hour: "2-digit",
    minute: "2-digit",
  });
}

// Consecutive messages from one sender within this window collapse under a
// single avatar + name header (Discord-style grouping).
export const GROUP_WINDOW_MS = 5 * 60 * 1000;
