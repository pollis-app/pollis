// Compact relative timestamp for conversation-list rows ("now", "5m", "2h",
// "3d", then a short date). Mobile counterpart to desktop's `utils/timeAgo`,
// shortened to fit a list row's trailing slot (no i18n layer on mobile yet).

export function timeAgoShort(input: string | number | Date): string {
  const d = input instanceof Date ? input : new Date(input);
  const ts = d.getTime();
  if (Number.isNaN(ts)) {
    return "";
  }
  const diffSec = Math.max(0, Math.floor((Date.now() - ts) / 1000));
  if (diffSec < 60) {
    return "now";
  }
  const diffMin = Math.floor(diffSec / 60);
  if (diffMin < 60) {
    return `${diffMin}m`;
  }
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) {
    return `${diffHr}h`;
  }
  const diffDay = Math.floor(diffHr / 24);
  if (diffDay < 7) {
    return `${diffDay}d`;
  }
  return d
    .toLocaleDateString(undefined, { month: "short", day: "numeric" })
    .toUpperCase();
}
