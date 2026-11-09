// Members under a sidebar voice channel (Discord's channel roster): name plus speaking / muted / live glyphs.

import React from "react";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { MicOff, Monitor } from "lucide-react";

import { appStore } from "../../stores/appStore";
import { useVoiceParticipants } from "../../hooks/queries/useVoiceParticipants";
import { Avatar } from "../ui/Avatar";
import { isMuted, isSpeaking } from "../../voice/participantAudio";
import { userIdFromVoiceIdentity } from "../../voice/identity";
import { voiceChannelId } from "../../types/voice-state";

interface VoiceChannelMembersProps {
  channelId: string;
  /** Fetch the roster; false when the room-count says the room is empty, so empty rooms cost no IPC. */
  enabled: boolean;
}

interface MemberRow {
  userId: string;
  name: string;
  avatarKey: string | null;
  speaking: boolean;
  muted: boolean;
  sharing: boolean;
}

/** One row per user; the local session's live state wins over the fetched roster for the room we are in. */
export const VoiceChannelMembers: React.FC<VoiceChannelMembersProps> = observer(({ channelId, enabled }) => {
  const { t } = useTranslation("voice");
  const { voiceState, voiceParticipants } = appStore;
  const inThisRoom = voiceChannelId(voiceState) === channelId && voiceState.kind !== "idle";
  const { data: roster = [] } = useVoiceParticipants(enabled && !inThisRoom ? channelId : null);

  const rows = new Map<string, MemberRow>();
  if (inThisRoom) {
    for (const p of voiceParticipants) {
      const userId = userIdFromVoiceIdentity(p.identity);
      const prev = rows.get(userId);
      const row: MemberRow = {
        userId,
        name: p.name,
        avatarKey: p.avatarKey ?? null,
        speaking: (prev?.speaking ?? false) || isSpeaking(p.audio),
        muted: prev ? prev.muted && isMuted(p.audio) : isMuted(p.audio),
        sharing: (prev?.sharing ?? false) || p.video.kind === "screenshare",
      };
      rows.set(userId, row);
    }
  } else {
    for (const p of roster) {
      const userId = userIdFromVoiceIdentity(p.identity);
      if (!rows.has(userId)) {
        rows.set(userId, {
          userId,
          name: p.name,
          avatarKey: p.avatar_url ?? null,
          speaking: false,
          muted: false,
          sharing: false,
        });
      }
    }
  }

  if (rows.size === 0) {
    return null;
  }

  return (
    <ul
      className="flex flex-col gap-0.5 py-0.5 ps-[3.6rem] pe-2.5"
      data-testid={`sidebar-voice-members-${channelId}`}
      aria-label={t("sidebar.membersLabel")}
    >
      {Array.from(rows.values()).map((m) => (
        <li
          key={m.userId}
          data-testid={`sidebar-voice-member-${channelId}-${m.userId}`}
          data-speaking={m.speaking ? "true" : undefined}
          data-muted={m.muted ? "true" : undefined}
          className={`flex items-center gap-1.5 text-sm leading-tight ${m.speaking ? "text-accent" : "text-dim"}`}
        >
          {/* A flex box, so it is exactly the avatar: as a block it took the row's line height and the ring stretched (#1789). */}
          <span className={`flex shrink-0 rounded-full ${m.speaking ? "ring-2 ring-accent" : ""}`}>
            <Avatar avatarKey={m.avatarKey} size={16} alt="" />
          </span>
          <bdi className="min-w-0 flex-1 truncate">{m.name}</bdi>
          {m.sharing && (
            <span className="inline-flex shrink-0 text-danger" title={t("sidebar.memberLive")} aria-label={t("sidebar.memberLive")}>
              <Monitor size={12} />
            </span>
          )}
          {m.muted && (
            <span className="inline-flex shrink-0 text-muted" title={t("sidebar.memberMuted")} aria-label={t("sidebar.memberMuted")}>
              <MicOff size={12} />
            </span>
          )}
        </li>
      ))}
    </ul>
  );
});
