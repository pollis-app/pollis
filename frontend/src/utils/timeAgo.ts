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
    return `${diffSec}s`;
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
  const diffWk = Math.floor(diffDay / 7);
  if (diffWk < 5) {
    return `${diffWk}w`;
  }
  const diffMo = Math.floor(diffDay / 30);
  if (diffMo < 12) {
    return `${diffMo}mo`;
  }
  const diffYr = Math.floor(diffDay / 365);
  return `${diffYr}y`;
}
