import React, { useEffect, useRef } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ArrowLeft, Circle, Volume2 } from "lucide-react";
import { useAppStore } from "../stores/appStore";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { VoiceChannelView } from "../components/Voice/VoiceChannelView";
import { useVoiceParticipants } from "../hooks/queries/useVoiceParticipants";
import { usePreferences } from "../hooks/queries/usePreferences";
import { Button } from "../components/ui/Button";
import { warmVoiceChannel } from "../utils/voiceWarmup";

export const VoiceChannelPage: React.FC = () => {
  const navigate = useNavigate();
  const { groupId, channelId } = useParams({ from: "/groups/$groupId/voice/$channelId" });
  const { activeVoiceChannelId, setActiveVoiceChannelId } = useAppStore();

  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const channel = group?.channels.find((c) => c.id === channelId);
  const channelName = channel?.name ?? "general";

  const preferences = usePreferences();

  const isInCall = activeVoiceChannelId === channelId;
  const { data: observerParticipants = [] } = useVoiceParticipants(isInCall ? null : channelId);

  // Autofocus the Join/Leave button on page entry.
  const joinLeaveRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    joinLeaveRef.current?.focus();
  }, []);

  // Issue #176: arriving on this page is intent to maybe join. Warm DNS/TLS
  // + token now so clicking Join is one round trip instead of cold-start.
  useEffect(() => {
    if (!isInCall) {
      warmVoiceChannel(channelId);
    }
  }, [channelId, isInCall]);

  // Auto-join once when preferences load, if the preference is enabled.
  const hasAutoJoined = useRef(false);
  useEffect(() => {
    if (hasAutoJoined.current || isInCall || !preferences.query.data) {
      return;
    }
    if (preferences.query.data.auto_join_voice === true) {
      hasAutoJoined.current = true;
      setActiveVoiceChannelId(channelId);
    }
  }, [preferences.query.data]);


  return (
    <div className="flex flex-col h-full font-mono text-xs">
      {/* Header */}
      <div
        className="flex items-center px-4 py-2 flex-shrink-0"
        style={{ borderBottom: "1px solid var(--c-border)", color: "var(--c-text-muted)" }}
      >
        <button
          onClick={() => navigate({ to: "/groups/$groupId", params: { groupId } })}
          className="mr-3 inline-flex items-center gap-1 leading-none transition-colors"
          style={{ color: "var(--c-text-muted)" }}
          onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-accent)"; }}
          onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-text-muted)"; }}
        >
          <ArrowLeft size={12} />
        </button>
        <span style={{ flex: 1, color: "var(--c-accent)" }} className="flex items-center gap-1.5">
          <Volume2 size={12} />
          {channelName}
        </span>
      </div>

      {/* Join / Leave button */}
      <div className="px-4 pt-4 pb-4 flex-shrink-0">
        <button
          ref={joinLeaveRef}
          data-testid="voice-join-leave-button"
          onClick={() => isInCall ? setActiveVoiceChannelId(null) : setActiveVoiceChannelId(channelId)}
          style={{
            background: isInCall ? "transparent" : "var(--c-accent)",
            color: isInCall ? "#ff6b6b" : "black",
            border: isInCall ? "2px solid #ff6b6b" : "2px solid transparent",
            padding: "8px 20px",
            fontFamily: "inherit",
            fontSize: "inherit",
            fontWeight: "bold",
            cursor: "pointer",
            letterSpacing: "0.05em",
            borderRadius: "0.25rem",
          }}
        >
          {isInCall ? "Leave" : "Join"}
        </button>
      </div>

      {/* Participant list */}
      {isInCall ? (
        <VoiceChannelView />
      ) : (
        <div
          className="flex-1 overflow-auto px-4 py-2 flex flex-col gap-1 font-mono text-xs"
          style={{ borderTop: "1px solid var(--c-border)", borderBottom: "1px solid var(--c-border)" }}
        >
          {observerParticipants.length === 0 ? (
            <span style={{ color: "var(--c-text-dim)" }}>No one in this channel</span>
          ) : (
            observerParticipants.map((p) => (
              <div
                key={p.identity}
                className="flex items-center gap-2"
                style={{ color: "var(--c-text)", borderLeft: "2px solid transparent", paddingLeft: "6px" }}
              >
                <span
                  className="text-lg"
                  style={{ color: "var(--c-border)", lineHeight: 1.25, flexShrink: 0, display: "flex", alignItems: "center" }}
                >
                  <Circle size={12} fill="var(--c-border)" />
                </span>
                <span className="flex-1 truncate">{p.name}</span>
              </div>
            ))
          )}
        </div>
      )}

      {/* Voice settings link */}
      <div className="px-4 py-3 flex-shrink-0">
        <Button variant="secondary" onClick={() => navigate({ to: "/voice-settings" })}>
          Voice Settings
        </Button>
      </div>
    </div>
  );
};
