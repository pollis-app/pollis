import React from "react";
import { useAppStore } from "../../stores/appStore";
import { NavigableGrid } from "../ui/NavigableGrid";
import type { VoiceParticipant } from "../../types";
import { VoiceMemberTile } from "./VoiceMemberTile";
import { LOCAL_PREVIEW_KEY } from "../../screenshare/screenShareSession";
import { ScreenSharePicker } from "./ScreenSharePicker";

export const VoiceChannelView: React.FC = () => {
  const {
    voiceParticipants,
    voiceActiveSpeakerIds,
    voicePhase,
    screenShareRemotes,
    screenShareLocalActive,
    screenShareLocalDimensions,
    screenShareMode,
    currentUser,
    setViewingScreenShareTrackKey,
  } = useAppStore();
  const localIdentity = currentUser ? `voice-${currentUser.id}` : null;

  // When the user is picking a screen-share source we take over the
  // entire channel content area with the in-app picker — no modal, no
  // overlay. This is the project's blanket "no modals" rule
  // (CLAUDE.md). Returns to the participant grid on cancel / start.
  if (screenShareMode === "picking") {
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
          const isLocal = p.identity === localIdentity;
          // Enter activates the streaming user's tile → open fullscreen.
          if (isLocal && screenShareLocalActive) {
            setViewingScreenShareTrackKey(LOCAL_PREVIEW_KEY);
            return;
          }
          const share = screenShareRemotes[p.identity];
          if (share) {
            setViewingScreenShareTrackKey(share.trackKey);
          }
        }}
        renderCell={(p, { focused }) => {
          const isLocal = p.identity === localIdentity;
          // Resolve which (if any) stream this participant is publishing
          // and pass it down as a unified shape so the tile doesn't care
          // whether it's our own preview track or a remote's.
          let streamTrackKey: string | undefined;
          let streamWidth: number | undefined;
          let streamHeight: number | undefined;
          if (isLocal && screenShareLocalActive) {
            streamTrackKey = LOCAL_PREVIEW_KEY;
            streamWidth = screenShareLocalDimensions?.width;
            streamHeight = screenShareLocalDimensions?.height;
          } else if (!isLocal) {
            const remote = screenShareRemotes[p.identity];
            if (remote) {
              streamTrackKey = remote.trackKey;
              streamWidth = remote.width;
              streamHeight = remote.height;
            }
          }
          return (
            <VoiceMemberTile
              identity={p.identity}
              name={p.name}
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
              isConnecting={isLocal && voicePhase === "joining"}
              onView={(trackKey) => setViewingScreenShareTrackKey(trackKey)}
            />
          );
        }}
      />
    </div>
  );
};
