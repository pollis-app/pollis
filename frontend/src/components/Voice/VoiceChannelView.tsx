import React from "react";
import { observer } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { NavigableGrid } from "../ui/NavigableGrid";
import type { VoiceParticipant } from "../../types";
import { VoiceMemberTile } from "./VoiceMemberTile";
import { LOCAL_PREVIEW_KEY } from "../../screenshare/screenShareSession";
import { ScreenSharePicker } from "./ScreenSharePicker";
import { shareOf } from "../../types/voice-state";
import { disambiguateVoiceNames } from "../../voice/disambiguateNames";
import { voiceUserKey } from "../../voice/identity";

export const VoiceChannelView: React.FC = observer(() => {
  const {
    voiceParticipants,
    voiceActiveSpeakerIds,
    voiceState,
    screenShareRemotes,
    setViewingScreenShareTrackKey,
  } = appStore;
  const share = shareOf(voiceState);
  const shareActive = share.kind === 'active';
  const shareLocalDims = shareActive ? share.dimensions : null;
  const isJoining = voiceState.kind === 'joining';
  // Suffix duplicate display names (`name`, `name (1)`, …) so two devices of
  // the same user are distinguishable in the grid (#140). Presentational only.
  const displayNames = disambiguateVoiceNames(voiceParticipants);

  // Navigating away from the voice/call view drops any fullscreen stream.
  // The viewer lives in AppShell (global overlay) so it would otherwise stay
  // pinned over other pages when the user leaves this view without closing the
  // stream first. Runs on unmount only — `setViewingScreenShareTrackKey` is a
  // stable Zustand setter.
  React.useEffect(() => {
    return () => setViewingScreenShareTrackKey(null);
  }, [setViewingScreenShareTrackKey]);

  // When the user is picking a screen-share source we take over the
  // entire channel content area with the in-app picker — no modal, no
  // overlay. This is the project's blanket "no modals" rule
  // (CLAUDE.md). Returns to the participant grid on cancel / start.
  if (share.kind === "picking") {
    return <ScreenSharePicker />;
  }

  return (
    <div
      className="flex-1 flex flex-col font-mono text-xs min-h-0"
      style={{
        borderTop: "1px solid var(--c-border)",
        borderBottom: "1px solid var(--c-border)",
      }}
    >
      <NavigableGrid<VoiceParticipant>
        items={voiceParticipants}
        getKey={(p) => p.identity}
        testId="voice-channel-view"
        emptyLabel="Connecting…"
        autoFocus={false}
        // Discord-ish sizing: a hard ceiling on tile width so a solo
        // user gets a sensibly-sized avatar tile (~240px) instead of
        // ballooning to fill the room. A crowded room squeezes tiles
        // down to the floor and then wraps to extra rows.
        minCellWidth={180}
        maxCellWidth={240}
        onActivate={(p) => {
          const isLocal = p.isLocal;
          // Enter activates the streaming user's tile → open fullscreen.
          if (isLocal && shareActive) {
            setViewingScreenShareTrackKey(LOCAL_PREVIEW_KEY);
            return;
          }
          // Per-device first (voice-{userId}:{deviceId}) so each device's
          // tile shows only its own share when the same user is in the
          // room from multiple devices. Fall back to the user-scoped key
          // (voice-{userId}) for cross-version compat with any client
          // still publishing under the legacy `{userId}:view` identity.
          const share =
            screenShareRemotes[p.identity] ??
            screenShareRemotes[voiceUserKey(p.identity)];
          if (share) {
            setViewingScreenShareTrackKey(share.trackKey);
          }
        }}
        renderCell={(p, { focused }) => {
          const isLocal = p.isLocal;
          // Resolve which (if any) stream this participant is publishing
          // and pass it down as a unified shape so the tile doesn't care
          // whether it's our own preview track or a remote's.
          let streamTrackKey: string | undefined;
          let streamWidth: number | undefined;
          let streamHeight: number | undefined;
          if (isLocal && shareActive) {
            streamTrackKey = LOCAL_PREVIEW_KEY;
            streamWidth = shareLocalDims?.width;
            streamHeight = shareLocalDims?.height;
          } else if (!isLocal) {
            const remote =
              screenShareRemotes[p.identity] ??
              screenShareRemotes[voiceUserKey(p.identity)];
            if (remote) {
              streamTrackKey = remote.trackKey;
              streamWidth = remote.width;
              streamHeight = remote.height;
            }
          }
          return (
            <VoiceMemberTile
              identity={p.identity}
              name={displayNames.get(p.identity) ?? p.name}
              avatarKey={p.avatarKey ?? null}
              isMuted={p.isMuted}
              isLocal={isLocal}
              isSpeaking={
                voiceActiveSpeakerIds.includes(p.identity) && !p.isMuted
              }
              focused={focused}
              connectionQuality={p.connectionQuality}
              streamTrackKey={streamTrackKey}
              streamWidth={streamWidth}
              streamHeight={streamHeight}
              // Connecting indicator only on the local user's own tile,
              // and only while the session is still negotiating with
              // LiveKit. Driven by VoiceSessionManager's phase, so it
              // clears the instant join_voice_channel resolves.
              isConnecting={isLocal && isJoining}
              onView={(trackKey) => setViewingScreenShareTrackKey(trackKey)}
            />
          );
        }}
      />
    </div>
  );
});
