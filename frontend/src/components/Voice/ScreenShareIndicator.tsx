// Compact broadcasting indicator that appears in the participant row
// when someone is sharing their screen. Local share renders a passive
// "LIVE" badge; remote share renders a clickable [▶ View Stream]
// pill with the live FPS readout next to it.
//
// We keep this inline (not a sub-row) so the row still works with
// NavigableList keyboard navigation. Stats come from the screen-share
// session frame stream — no extra IPC, no polling timer.

import React from "react";
import { Play, Radio } from "lucide-react";
import { PillButton } from "../ui/PillButton";
import { useScreenShareStats } from "../../screenshare/useScreenShareStats";

interface RemoteShareInfo {
  trackKey: string;
  width: number;
  height: number;
}

interface Props {
  identity: string;
  isLocal: boolean;
  remote: RemoteShareInfo | undefined;
  onView: (trackKey: string) => void;
}

export const ScreenShareIndicator: React.FC<Props> = ({
  identity,
  isLocal,
  remote,
  onView,
}) => {
  const stats = useScreenShareStats(remote?.trackKey ?? null);
  if (!isLocal && !remote) {
    return null;
  }

  if (isLocal && !remote) {
    // Local-only: passive "you're broadcasting" badge. No view button —
    // a user can't watch their own stream meaningfully.
    return (
      <span
        data-testid={`voice-screenshare-local-${identity}`}
        className="flex items-center gap-1 font-mono text-[10px] flex-shrink-0"
        style={{
          color: "var(--c-accent)",
          padding: "1px 6px",
          border: "1px solid var(--c-accent)",
          borderRadius: 3,
          letterSpacing: "0.05em",
        }}
        title="You are sharing your screen"
      >
        <Radio size={10} className="animate-pulse" />
        LIVE
      </span>
    );
  }

  const trackKey = remote!.trackKey;
  const w = stats.dimensions?.width ?? remote!.width;
  const h = stats.dimensions?.height ?? remote!.height;
  // Compact stat string: WxH @ Nfps. Resolution drops to a height-only
  // shorthand (720p, 1080p) when it matches a common rung, otherwise we
  // print the actual height — wins line space and reads quicker.
  const heightLabel = h % 90 === 0 && h <= 4320 ? `${h}p` : `${h}px`;
  const fpsLabel = stats.fps > 0 ? `${stats.fps}fps` : "…";

  return (
    <span className="flex items-center gap-2 flex-shrink-0">
      <PillButton
        accent="var(--c-accent)"
        data-testid={`voice-screenshare-${identity}`}
        aria-label={`View screen share from ${identity}`}
        title={`View ${w}×${h} screen share`}
        onClick={() => onView(trackKey)}
      >
        <Play size={10} fill="currentColor" />
        <span className="text-[10px]">View Stream</span>
      </PillButton>
      <span
        className="font-mono text-[10px] tabular-nums"
        style={{ color: "var(--c-text-dim)" }}
        data-testid={`voice-screenshare-stats-${identity}`}
      >
        {heightLabel} {fpsLabel}
      </span>
    </span>
  );
};
