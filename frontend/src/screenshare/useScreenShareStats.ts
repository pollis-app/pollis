import { useEffect, useState } from "react";
import { screenShareSession, type FrameStats } from "./screenShareSession";

const EMPTY: FrameStats = { fps: 0, dimensions: null, lastFrameBytes: 0 };

/** Subscribes to per-frame stats for a screen-share track. Updates on
 *  every arriving frame (no internal timer) — onStats replays the last
 *  known value on subscribe so callers don't render an empty state for
 *  a frame interval. */
export function useScreenShareStats(trackKey: string | null): FrameStats {
  const [stats, setStats] = useState<FrameStats>(EMPTY);
  useEffect(() => {
    if (!trackKey) {
      setStats(EMPTY);
      return;
    }
    return screenShareSession.onStats(trackKey, setStats);
  }, [trackKey]);
  return stats;
}
