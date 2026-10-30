/*
 * Where a safety-number change notice (#1605) sits in a DM timeline. One rule,
 * two copies: this file and mobile/lib/keyChangeTimeline.ts
 * (frontend/tests/key-change-timeline.test.ts pins both and that they match).
 */

export interface TimedNotice {
  id: number;
  peer_user_id: string;
  detected_at: string;
}

export interface PlacedNotice<N extends TimedNotice> {
  notice: N;
  at: number;
}

/**
 * The notices that belong inside the loaded window, with their time in ms,
 * oldest first. A notice older than the oldest loaded message waits until
 * that page loads (`hasOlder`), and one newer than the newest waits for the
 * newer page of an anchored window (`hasNewer`); otherwise it would jump to
 * the edge of whatever happens to be loaded. With nothing loaded on either
 * side it shows. An unreadable timestamp never shows: there is no honest
 * position for it.
 */
export function placeKeyChangeNotices<N extends TimedNotice>(
  notices: readonly N[],
  oldestMs: number | null,
  newestMs: number | null,
  hasOlder: boolean,
  hasNewer: boolean,
): PlacedNotice<N>[] {
  const out: PlacedNotice<N>[] = [];
  for (const notice of notices) {
    const at = Date.parse(notice.detected_at);
    if (!Number.isFinite(at)) {
      continue;
    }
    if (hasOlder && oldestMs !== null && at < oldestMs) {
      continue;
    }
    if (hasNewer && newestMs !== null && at > newestMs) {
      continue;
    }
    out.push({ notice, at });
  }
  out.sort((a, b) => a.at - b.at || a.notice.id - b.notice.id);
  return out;
}
