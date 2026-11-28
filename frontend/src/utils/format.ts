import i18n from "../i18n";

// Index = power of 1024; order matters.
const FILE_SIZE_UNIT_KEYS = [
  "chat:fileSize.bytes",
  "chat:fileSize.kilobytes",
  "chat:fileSize.megabytes",
  "chat:fileSize.gigabytes",
] as const;

export function formatFileSize(bytes: number): string {
  if (bytes === 0) { return ""; }
  const power = Math.floor(Math.log(bytes) / Math.log(1024));
  const i = Math.min(Math.max(power, 0), FILE_SIZE_UNIT_KEYS.length - 1);
  const size = parseFloat((bytes / Math.pow(1024, i)).toFixed(1));
  return i18n.t(FILE_SIZE_UNIT_KEYS[i], { size });
}

export function formatDuration(seconds: number): string {
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${String(s).padStart(2, "0")}`;
}

/** The app language, never the OS one; every `toLocale*` / `Intl.*` call must pass this. Read at call time. */
export function activeLocale(): string {
  // The language i18next actually selected after fallback.
  return i18n.resolvedLanguage || i18n.language || "en";
}

// One formatter per (locale, options) (#2167): `toLocale*String(locale, options)` builds a new one on every call.
const dateFormats = new Map<string, Intl.DateTimeFormat>();

function dateFormat(key: string, options: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
  const locale = activeLocale();
  const cacheKey = `${locale}|${key}`;
  let format = dateFormats.get(cacheKey);
  if (!format) {
    format = new Intl.DateTimeFormat(locale, options);
    dateFormats.set(cacheKey, format);
  }
  return format;
}

/** Drop the cached formatters: a formatter keeps the time zone it was built in. */
function clearDateFormats(): void {
  dateFormats.clear();
}

i18n.on("languageChanged", clearDateFormats);
// A focus may follow an OS time-zone change; the next format picks the new zone up.
if (typeof window !== "undefined") {
  window.addEventListener("focus", clearDateFormats);
}

/** Normalise `created_at` to epoch ms; below `1e12` it must be seconds (#874). */
export function toMs(timestamp: number): number {
  return timestamp < 1e12 ? timestamp * 1000 : timestamp;
}

// Time-of-day label, e.g. "3:07 PM". Expects epoch milliseconds.
export function formatTimeOfDay(ms: number): string {
  return dateFormat("time", { hour: "numeric", minute: "2-digit" }).format(ms);
}

/** Zero-padded clock time, e.g. "03:07 PM", for the thread panel's narrow column. */
export function formatClockTime(ms: number): string {
  return dateFormat("clock", { hour: "2-digit", minute: "2-digit" }).format(ms);
}

// Full date + time, used for hover tooltips. Expects epoch milliseconds.
export function formatFullTimestamp(ms: number): string {
  return dateFormat("full", { dateStyle: "full", timeStyle: "short" }).format(ms);
}

// "Today" / "Yesterday" / weekday / date. Expects epoch ms.
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
    return dateFormat("dayWeek", { weekday: "short", month: "short", day: "numeric" }).format(d);
  }
  if (d.getFullYear() === now.getFullYear()) {
    return dateFormat("dayMonth", { month: "short", day: "numeric" }).format(d);
  }
  return dateFormat("dayYear", { month: "short", day: "numeric", year: "numeric" }).format(d);
}

// Date + time from an ISO string; the raw input on failure.
export function formatDateTime(iso: string): string {
  try {
    const d = new Date(iso);
    return d.toLocaleString(activeLocale());
  } catch {
    return iso;
  }
}

// Short date + time from an ISO string; the raw input on failure.
export function formatShortDateTime(iso: string): string {
  const d = new Date(iso);
  if (isNaN(d.getTime())) {
    return iso;
  }
  return d.toLocaleString(activeLocale(), {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}
