import React from "react";
import { useAppStore } from "../../stores/appStore";
import { NavigableGrid } from "../ui/NavigableGrid";
import type { VoiceParticipant } from "../../types";
import { VoiceMemberTile } from "./VoiceMemberTile";
import { LocalSharePreview } from "./LocalSharePreview";

export const VoiceChannelView: React.FC = () => {
  const {
    voiceParticipants,
    voiceActiveSpeakerIds,
    screenShareRemotes,
    screenShareLocalActive,
    currentUser,
    setViewingScreenShareTrackKey,
  } = useAppStore();
  const localIdentity = currentUser ? `voice-${currentUser.id}` : null;

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
        onActivate={(p) => {
          const share = screenShareRemotes[p.identity];
          if (share) {
            setViewingScreenShareTrackKey(share.trackKey);
          }
        }}
        renderCell={(p, { focused }) => {
          const isLocal = p.identity === localIdentity;
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
              remoteShare={isLocal ? undefined : screenShareRemotes[p.identity]}
              isLocalBroadcasting={isLocal && screenShareLocalActive}
              onView={(trackKey) => setViewingScreenShareTrackKey(trackKey)}
            />
          );
        }}
      />
      <LocalSharePreview />
    </div>
  );
};
