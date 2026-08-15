import i18n from "../i18n";

// Unit suffix keys, smallest first. The index is the power of 1024, so the
// list order is load-bearing.
const FILE_SIZE_UNIT_KEYS = [
  "chat:fileSize.bytes",
  "chat:fileSize.kilobytes",
  "chat:fileSize.megabytes",
  "chat:fileSize.gigabytes",
] as const;

export function formatFileSize(bytes: number): string {
  if (bytes === 0) { return ""; }
  const power = Math.floor(Math.log(bytes) / Math.log(1024));
  // Anything past GB keeps the largest unit rather than running off the end
  // of the table.
  const i = Math.min(Math.max(power, 0), FILE_SIZE_UNIT_KEYS.length - 1);
  const size = parseFloat((bytes / Math.pow(1024, i)).toFixed(1));
  return i18n.t(FILE_SIZE_UNIT_KEYS[i], { size });
}

export function formatDuration(seconds: number): string {
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${String(s).padStart(2, "0")}`;
}

// Time-of-day label, e.g. "3:07 PM". Expects epoch milliseconds.
export function formatTimeOfDay(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

// Full date + time, used for hover tooltips. Expects epoch milliseconds.
export function formatFullTimestamp(ms: number): string {
  return new Date(ms).toLocaleString([], { dateStyle: "full", timeStyle: "short" });
}

// Day-divider label relative to today: "Today" / "Yesterday" / weekday /
// month-day (/ year for prior years). Expects epoch milliseconds.
export function formatDayDivider(ms: number): string {
  const startOfLocalDay = (d: Date): number =>
    new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const d = new Date(ms);
  const now = new Date();
  const dayStart = startOfLocalDay(d);
  const todayStart = startOfLocalDay(now);
  const dayDiff = Math.round((todayStart - dayStart) / 86_400_000);

  if (dayDiff === 0) {
    return i18n.t("common:time.today");
  }
  if (dayDiff === 1) {
    return i18n.t("common:time.yesterday");
  }
  if (dayDiff > 1 && dayDiff <= 6) {
    return d.toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" });
  }
  if (d.getFullYear() === now.getFullYear()) {
    return d.toLocaleDateString([], { month: "short", day: "numeric" });
  }
  return d.toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" });
}

// Locale-default date + time from an ISO string; returns the raw input if
// construction throws.
export function formatDateTime(iso: string): string {
  try {
    const d = new Date(iso);
    return d.toLocaleString();
  } catch {
    return iso;
  }
}

// Short date + time ("Jun 7, 03:07 PM") from an ISO string; returns the raw
// input if it cannot be parsed.
export function formatShortDateTime(iso: string): string {
  const d = new Date(iso);
  if (isNaN(d.getTime())) {
    return iso;
  }
  return d.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}
