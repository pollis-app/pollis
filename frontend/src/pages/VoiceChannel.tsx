import React, { useEffect, useRef } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { Pencil, Trash2, X } from "lucide-react";
import { observer } from "mobx-react-lite";
import { appStore } from "../stores/appStore";
import { useDeleteChannel, useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { VoiceStage } from "../components/Voice/stage/VoiceStage";
import { useVoiceParticipants } from "../hooks/queries/useVoiceParticipants";
import { usePreferences } from "../hooks/queries/usePreferences";
import { Button } from "../components/ui/Button";
import type { VoiceParticipant } from "../types";
import { warmVoiceChannel } from "../utils/voiceWarmup";
import { voiceSession } from "../voice";



export const VoiceChannelPage: React.FC = observer(() => {
  const navigate = useNavigate();
  const { groupId, channelId } = useParams({ from: "/groups/$groupId/voice/$channelId" });
  const {
    voiceState,
    pendingDeleteChannelId,
    setPendingDeleteChannelId,
  } = appStore;
  const activeVoiceChannelId =
    voiceState.kind === 'idle' ? null : voiceState.channelId;

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


  // Admin rename/delete affordances live in the stage header, right of
  // the pills and left of Join/Leave. Hidden while the delete bar is open.
  const headerActions =
    isAdmin && channel && !isPendingDelete ? (
      <>
        <button
          data-testid="rename-channel-trigger"
          onClick={() =>
            navigate({
              to: "/groups/$groupId/channels/$channelId/rename",
              params: { groupId, channelId },
            })
          }
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
      </>
    ) : null;

  // The pending-delete confirmation replaces the stage footer, following
  // the established replace-the-bar pattern (no modal).
  const deleteFooter = isPendingDelete ? (
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
  ) : undefined;

  return (
    <VoiceStage
      channelName={channelName}
      isInCall={isInCall}
      observerParticipants={observerParticipants.map((p): VoiceParticipant => ({
        identity: p.identity,
        name: p.name,
        avatarKey: p.avatar_url ?? null,
        isMuted: false,
        isLocal: false,
      }))}
      onJoin={() => voiceSession.setIntent({ channelId, groupId })}
      onLeave={() => voiceSession.leave()}
      onBack={() => navigate({ to: "/groups/$groupId", params: { groupId } })}
      onOpenSettings={() => navigate({ to: "/voice-settings" })}
      headerActions={headerActions}
      footer={deleteFooter}
    />
  );
});
