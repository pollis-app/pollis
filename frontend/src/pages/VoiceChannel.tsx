import React, { useEffect, useRef } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ArrowLeft, Circle, Pencil, Trash2, Volume2, X } from "lucide-react";
import { useAppStore } from "../stores/appStore";
import { useDeleteChannel, useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { VoiceChannelView } from "../components/Voice/VoiceChannelView";
import { useVoiceParticipants } from "../hooks/queries/useVoiceParticipants";
import { usePreferences } from "../hooks/queries/usePreferences";
import { Button } from "../components/ui/Button";
import { NavigableList } from "../components/ui/NavigableList";
import { Avatar } from "../components/ui/Avatar";
import { warmVoiceChannel } from "../utils/voiceWarmup";
import { voiceSession } from "../voice";

interface ObserverParticipant {
  identity: string;
  name: string;
  avatar_url?: string | null;
}



export const VoiceChannelPage: React.FC = () => {
  const navigate = useNavigate();
  const { groupId, channelId } = useParams({ from: "/groups/$groupId/voice/$channelId" });
  const {
    activeVoiceChannelId,
    pendingDeleteChannelId,
    setPendingDeleteChannelId,
  } = useAppStore();

  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const channel = group?.channels.find((c) => c.id === channelId);
  const channelName = channel?.name ?? "general";
  const isAdmin = group?.current_user_role === "admin";

  const preferences = usePreferences();
  const deleteChannelMutation = useDeleteChannel();
  const isPendingDelete = pendingDeleteChannelId === channelId;

  // Clear the pending-delete bar when navigating away from the channel.
  useEffect(() => {
    return () => {
      setPendingDeleteChannelId(null);
    };
  }, [channelId, setPendingDeleteChannelId]);

  // Esc cancels the pending-delete bar without leaving the page.
  useEffect(() => {
    if (!isPendingDelete) {
      return;
    }
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopImmediatePropagation();
        setPendingDeleteChannelId(null);
      }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true });
  }, [isPendingDelete, setPendingDeleteChannelId]);

  const handleConfirmDelete = async () => {
    try {
      await deleteChannelMutation.mutateAsync({ groupId, channelId });
      setPendingDeleteChannelId(null);
      navigate({ to: "/groups/$groupId", params: { groupId } });
    } catch (err) {
      console.error("Failed to delete voice channel:", err);
    }
  };

  const isInCall = activeVoiceChannelId === channelId;
  const { data: observerParticipants = [] } = useVoiceParticipants(isInCall ? null : channelId);

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
      voiceSession.setIntent({ channelId, groupId });
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
          className="mr-3 inline-flex items-center gap-1 leading-none transition-colors text-[var(--c-text-muted)] hover:text-[var(--c-accent)]"
        >
          <ArrowLeft size={12} />
        </button>
        <span style={{ flex: 1, color: "var(--c-accent)" }} className="flex items-center gap-1.5">
          <Volume2 size={12} />
          {channelName}
        </span>
        {isAdmin && channel && !isPendingDelete && (
          <div className="flex items-center gap-2">
            <button
              data-testid="rename-channel-trigger"
              onClick={() => navigate({ to: "/groups/$groupId/channels/$channelId/rename", params: { groupId, channelId } })}
              aria-label="Rename channel"
              className="icon-btn-sm flex-shrink-0"
            >
              <Pencil size={14} aria-hidden="true" />
            </button>
            <button
              data-testid="delete-channel-trigger"
              onClick={() => setPendingDeleteChannelId(channelId)}
              aria-label="Delete channel"
              className="icon-btn-sm flex-shrink-0"
            >
              <Trash2 size={14} aria-hidden="true" />
            </button>
          </div>
        )}
      </div>

      {/* Join / Leave button */}
      <div className="px-4 pt-4 pb-4 flex-shrink-0">
        <Button
          data-testid="voice-join-leave-button"
          variant={isInCall ? "danger" : "primary"}
          autoFocus
          onClick={() => isInCall ? voiceSession.leave() : voiceSession.setIntent({ channelId, groupId })}
        >
          {isInCall ? "Leave" : "Join"}
        </Button>
      </div>

      {/* Participant list */}
      {isInCall ? (
        <VoiceChannelView />
      ) : (
        <div
          className="flex-1 flex flex-col font-mono text-xs"
          style={{
            borderTop: "1px solid var(--c-border)",
            borderBottom: "1px solid var(--c-border)",
          }}
        >
          <NavigableList<ObserverParticipant>
            items={observerParticipants}
            getKey={(p) => p.identity}
            autoFocus={false}
            emptyLabel="No one in this channel"
            renderRow={(p) => (
              <>
                <span
                  className="text-lg"
                  style={{
                    color: "var(--c-border)",
                    lineHeight: 1.25,
                    flexShrink: 0,
                    display: "flex",
                    alignItems: "center",
                  }}
                >
                  <Circle size={12} fill="var(--c-border)" />
                </span>
                <Avatar
                  avatarKey={p.avatar_url ?? null}
                  size={20}
                  alt={p.name}
                  testId={`voice-observer-avatar-${p.identity}`}
                />
                <span className="flex-1 truncate">{p.name}</span>
              </>
            )}
          />
        </div>
      )}

      {/* Voice settings link, replaced by the delete-confirm bar when an
          admin has triggered channel deletion. */}
      {isPendingDelete ? (
        <div data-testid="delete-channel-bar">
          <div
            className="flex items-center gap-2 px-4 py-1.5 flex-shrink-0"
            style={{ borderTop: "1px solid var(--c-border)", background: "var(--c-surface)" }}
          >
            <span className="flex-1 text-2xs font-mono uppercase tracking-widest" style={{ color: "var(--c-text-muted)" }}>
              delete channel
            </span>
            <button
              data-testid="delete-channel-cancel"
              onClick={() => setPendingDeleteChannelId(null)}
              aria-label="Cancel delete"
              className="icon-btn-sm flex-shrink-0"
            >
              <X size={20} aria-hidden="true" />
            </button>
          </div>
          <div
            className="flex items-center justify-between gap-4 px-4 pb-3 pt-2"
            style={{ background: "var(--c-surface)" }}
          >
            <p className="text-xs font-mono" style={{ color: "var(--c-text-dim)" }}>
              This voice channel and any in-call state will be permanently deleted. This cannot be undone.
            </p>
            <Button
              data-testid="delete-channel-confirm"
              variant="danger"
              onClick={handleConfirmDelete}
              isLoading={deleteChannelMutation.isPending}
              loadingText="Deleting…"
              autoFocus
            >
              Delete
            </Button>
          </div>
        </div>
      ) : (
        <div className="px-4 py-3 flex-shrink-0">
          <Button variant="secondary" onClick={() => navigate({ to: "/voice-settings" })}>
            Voice Settings
          </Button>
        </div>
      )}
    </div>
  );
};
