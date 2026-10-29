import { useEffect, useState } from "react";
import { screenShareSession, type FrameStats } from "./screenShareSession";

const EMPTY: FrameStats = { fps: 0, dimensions: null, lastFrameBytes: 0 };

/** Per-frame stats for a screen-share track (`screenShareSession.onStats`). */
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
