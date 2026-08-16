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

/**
 * The locale every `Intl`/`toLocale*` call in this module formats against:
 * the APP language, never the OS one.
 *
 * `toLocaleDateString([])` / `toLocaleString(undefined)` resolve to the host's
 * default locale, which under Tauri is the OS's — so an Arabic UI on an
 * English machine used to render "Mon, Jun 7" directly underneath a translated
 * "Today". The language the user picked in Preferences is the one they read.
 *
 * Read from the i18next singleton at CALL time rather than taken as a
 * parameter, exactly as `timeAgo.ts` and `formatFileSize` below already read
 * `i18n.t`. These are pure module functions called from a dozen components;
 * making them hooks would push a `useTranslation` into every caller and change
 * a dozen signatures to fix a formatting bug. The re-render already works out:
 * `changeLanguage` re-renders every component subscribed through
 * `useTranslation`, and every component that calls these helpers
 * (MessageList, MessageItem, SearchView, SecurityPage) already is.
 *
 * Exported because three components format a Date inline rather than through
 * a helper here (the invite rows and the thread panel), and every one of them
 * had the same OS-locale bug. **Any `toLocale*` or `Intl.*` call anywhere in
 * the renderer must pass this** — a bare `[]` or `undefined` argument is the
 * defect, not a default.
 */
export function activeLocale(): string {
  // `resolvedLanguage` is the one i18next actually selected after
  // fallback/normalization; `language` is the raw request. Prefer the former
  // so a tag we do not ship formats in the language actually on screen.
  return i18n.resolvedLanguage || i18n.language || "en";
}

/**
 * Normalise a `Message.created_at` to epoch **milliseconds**.
 *
 * `created_at` is documented as "the primary timestamp" and every producer is
 * *supposed* to hand over milliseconds, but the shape is a bare `number`, so a
 * seconds-precision value is indistinguishable at the type level and silently
 * renders as January 1970 — a wrong answer on screen, not a crash. Every
 * component that formats a message timestamp therefore normalises first.
 *
 * The cutover is `1e12` ms (2001-09-09). Anything below it cannot be a
 * plausible message time in milliseconds, and anything at or above it cannot
 * be a plausible one in seconds (year 33658), so the two ranges do not
 * overlap for any timestamp this app will ever hold.
 *
 * Lived in three copies (`MessageItem`, `MessageList`, and inline in
 * `timeAgo`) and was **missing** from the fourth caller, `ThreadMessageRow`,
 * which is exactly the failure mode a copy-pasted guard has (#874).
 */
export function toMs(timestamp: number): number {
  return timestamp < 1e12 ? timestamp * 1000 : timestamp;
}

// Time-of-day label, e.g. "3:07 PM". Expects epoch milliseconds.
export function formatTimeOfDay(ms: number): string {
  return new Date(ms).toLocaleTimeString(activeLocale(), {
    hour: "numeric",
    minute: "2-digit",
  });
}

/**
 * Zero-padded clock time, e.g. "03:07 PM" — the narrow-column shape the thread
 * panel uses, where a ragged `9:07` / `10:07` left edge is visible against the
 * author name beside it. Expects epoch milliseconds.
 */
export function formatClockTime(ms: number): string {
  return new Date(ms).toLocaleTimeString(activeLocale(), {
    hour: "2-digit",
    minute: "2-digit",
  });
}

// Full date + time, used for hover tooltips. Expects epoch milliseconds.
export function formatFullTimestamp(ms: number): string {
  return new Date(ms).toLocaleString(activeLocale(), {
    dateStyle: "full",
    timeStyle: "short",
  });
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

// Date + time in the app language from an ISO string; returns the raw input
// if construction throws.
export function formatDateTime(iso: string): string {
  try {
    const d = new Date(iso);
    return d.toLocaleString(activeLocale());
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
  return d.toLocaleString(activeLocale(), {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}
