// Day/time formatting helpers for the chat timeline.

import i18n, { activeLocale, upper } from "../../i18n";

export function dayKey(ts: number): string {
  return new Date(ts).toDateString();
}

export function dayLabel(ts: number): string {
  const d = new Date(ts);
  const today = new Date();
  if (d.toDateString() === today.toDateString()) {
    return upper(i18n.t("common:time.today"));
  }
  const yest = new Date(today);
  yest.setDate(today.getDate() - 1);
  if (d.toDateString() === yest.toDateString()) {
    return upper(i18n.t("common:time.yesterday"));
  }
  return upper(
    d.toLocaleDateString(activeLocale(), { month: "short", day: "numeric" }),
  );
}

export function timeLabel(ts: number): string {
  return new Date(ts).toLocaleTimeString(activeLocale(), {
    hour: "2-digit",
    minute: "2-digit",
  });
}
