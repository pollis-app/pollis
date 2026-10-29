import i18n from "../i18n";

// Smallest first: the index is the power of 1024.
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

/**
 * The locale every `Intl` / `toLocale*` call formats against: the APP language,
 * never the OS's. Read from i18next at call time, so these stay plain functions
 * (their callers already re-render on a language change). **Any `toLocale*` or
 * `Intl.*` call in the renderer must pass this**; a bare `[]` or `undefined` is
 * the bug.
 */
export function activeLocale(): string {
  // `resolvedLanguage` is what i18next selected after fallback.
  return i18n.resolvedLanguage || i18n.language || "en";
}

/**
 * Normalise a `Message.created_at` to epoch ms: a seconds value would render
 * as 1970 (#874). Values below `1e12` are seconds; the ranges can't overlap
 * for any real message time.
 */
export function toMs(timestamp: number): number {
  return timestamp < 1e12 ? timestamp * 1000 : timestamp;
}

// "3:07 PM", from epoch ms.
export function formatTimeOfDay(ms: number): string {
  return new Date(ms).toLocaleTimeString(activeLocale(), {
    hour: "numeric",
    minute: "2-digit",
  });
}

/** Zero-padded clock time ("03:07 PM"), from epoch ms, so a narrow column's edge stays even. */
export function formatClockTime(ms: number): string {
  return new Date(ms).toLocaleTimeString(activeLocale(), {
    hour: "2-digit",
    minute: "2-digit",
  });
}

// Full date + time for tooltips, from epoch ms.
export function formatFullTimestamp(ms: number): string {
  return new Date(ms).toLocaleString(activeLocale(), {
    dateStyle: "full",
    timeStyle: "short",
  });
}

// Day-divider label: Today / Yesterday / weekday / month-day (/ year), from epoch ms.
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
    return d.toLocaleDateString(activeLocale(), {
      weekday: "short",
      month: "short",
      day: "numeric",
    });
  }
  if (d.getFullYear() === now.getFullYear()) {
    return d.toLocaleDateString(activeLocale(), { month: "short", day: "numeric" });
  }
  return d.toLocaleDateString(activeLocale(), {
    month: "short",
    day: "numeric",
    year: "numeric",
  });
}

// Date + time from an ISO string; the raw input if it can't be formatted.
export function formatDateTime(iso: string): string {
  try {
    const d = new Date(iso);
    return d.toLocaleString(activeLocale());
  } catch {
    return iso;
  }
}

// Short date + time ("Jun 7, 03:07 PM") from an ISO string; raw input if unparsable.
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
