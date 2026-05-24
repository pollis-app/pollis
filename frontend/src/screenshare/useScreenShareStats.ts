import { useEffect, useState } from "react";
import { hasElectron } from "../bridge";
import { livekitView } from "./livekitView";
import { screenShareSession, type FrameStats } from "./screenShareSession";

const EMPTY: FrameStats = { fps: 0, dimensions: null, lastFrameBytes: 0 };

/** Subscribes to per-frame stats for a screen-share track. Source depends
 *  on runtime:
 *    - Electron: `livekitView` records FPS + dimensions via
 *      `requestVideoFrameCallback` on the <video> element (see
 *      RemoteVideoTile). Real-time native browser metrics.
 *    - Tauri: `screenShareSession.onStats` derived from the Rust frame
 *      channel (the legacy path).
 *  Either way the returned shape is identical so VoiceMemberTile's
 *  stats label keeps working unchanged. */
export function useScreenShareStats(trackKey: string | null): FrameStats {
  const [stats, setStats] = useState<FrameStats>(EMPTY);
  useEffect(() => {
    if (!trackKey) {
      setStats(EMPTY);
      return;
    }
    if (hasElectron()) {
      return livekitView.onStats(trackKey, (s) => {
        setStats({
          fps: s.fps,
          dimensions: s.width && s.height
            ? { width: s.width, height: s.height }
            : null,
          // Electron path doesn't track bytes/frame — set 0; the
          // VoiceMemberTile statsLabel doesn't surface bytes anyway.
          lastFrameBytes: 0,
        });
      });
    }
    return screenShareSession.onStats(trackKey, setStats);
  }, [trackKey]);
  return stats;
}
