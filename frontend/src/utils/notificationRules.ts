// The desktop's read and activity decisions (#2056), import-free so node tests cover them.
// The rules they implement are written up in .codesight/wiki/notifications.md → "Fire and clear rules".

/** Renew an active report at most this often; well inside the DS's two-minute lapse. */
export const RENEW_MS = 60 * 1000;

/**
 * Mark the conversation on screen read? Only while the user is here (focused, visible, not idle),
 * something is unread, and the newest message is in view: the log is at its live tail and
 * scrolled to within `thresholdPx` of the bottom.
 */
export function shouldMarkRead(args: {
  active: boolean;
  unread: number;
  atLiveTail: boolean;
  fromBottomPx: number;
  thresholdPx: number;
}): boolean {
  return args.active && args.unread > 0 && args.atLiveTail && args.fromBottomPx <= args.thresholdPx;
}

/**
 * Send an activity report now? Always on a change; while active, again once `RENEW_MS` has passed
 * (driven by input, never a timer); never repeat an inactive report.
 */
export function activityReportDue(
  prev: { active: boolean; sentAt: number } | null,
  active: boolean,
  now: number,
): boolean {
  if (!prev || prev.active !== active) {
    return true;
  }
  return active && now - prev.sentAt >= RENEW_MS;
}
