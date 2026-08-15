import i18n from "../i18n";

// Short, human-readable "time ago" string.
// Accepts ISO strings, epoch ms, or epoch s.
export function timeAgo(input: string | number | Date): string {
  const d = input instanceof Date ? input : new Date(
    typeof input === "number" && input < 1e12 ? input * 1000 : input,
  );
  const ts = d.getTime();
  if (Number.isNaN(ts)) {
    return "";
  }
  const diffSec = Math.max(0, Math.floor((Date.now() - ts) / 1000));
  if (diffSec < 60) {
    return i18n.t("common:timeAgo.seconds", { count: diffSec });
  }
  const diffMin = Math.floor(diffSec / 60);
  if (diffMin < 60) {
    return i18n.t("common:timeAgo.minutes", { count: diffMin });
  }
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) {
    return i18n.t("common:timeAgo.hours", { count: diffHr });
  }
  const diffDay = Math.floor(diffHr / 24);
  if (diffDay < 7) {
    return i18n.t("common:timeAgo.days", { count: diffDay });
  }
  const diffWk = Math.floor(diffDay / 7);
  if (diffWk < 5) {
    return i18n.t("common:timeAgo.weeks", { count: diffWk });
  }
  const diffMo = Math.floor(diffDay / 30);
  if (diffMo < 12) {
    return i18n.t("common:timeAgo.months", { count: diffMo });
  }
  const diffYr = Math.floor(diffDay / 365);
  return i18n.t("common:timeAgo.years", { count: diffYr });
}
