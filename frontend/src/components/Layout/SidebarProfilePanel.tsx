import React from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { useRouter } from "@tanstack/react-router";
import { Settings as SettingsIcon, Volume2, Monitor, MonitorOff, PhoneOff } from "lucide-react";
import { appStore } from "../../stores/appStore";
import { useUserProfile } from "../../hooks/queries/useUserProfile";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { Avatar } from "../ui/Avatar";
import { voiceSession } from "../../voice";
import { SidebarVoiceControls } from "../Voice/SidebarVoiceControls";
import { toggleScreenShare } from "../../screenshare/screenShareActions";
import { shareOf } from "../../types/voice-state";

/**
 * Discord-style identity panel anchored to the sidebar bottom (refined skin).
 * Row 1 is always shown: avatar (with online dot) + display name + @username +
 * a settings gear. Row 2 appears only while connected to voice — the persistent
 * voice-status strip (channel + mic + deafen + screenshare + disconnect) that
 * the standalone terminal `VoiceBar` provides. In refined, AppShell hides that
 * bar and this strip owns the persistent voice controls instead, so it has to
 * carry the full set: the mic and deafen halves live in `SidebarVoiceControls`,
 * which shares its state mapping and labels with `VoiceBar` and the
 * `VoiceStage` tray. No participant count (per the design brief).
 */
export const SidebarProfilePanel: React.FC = observer(() => {
  const { t } = useTranslation("nav");
  // The return-to-voice labels are shared with terminal's VoiceBar.
  const { t: tVoice } = useTranslation("voice");
  const router = useRouter();
  const { data: profile } = useUserProfile();
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const { voiceState } = appStore;

  const displayName =
    profile?.preferred_name || profile?.username || t("profile.you");
  const handle = profile?.username ? `@${profile.username}` : null;

  const inVoice = voiceState.kind === "joined";
  const voiceChannelId = inVoice ? voiceState.channelId : null;
  const share = shareOf(voiceState);
  const shareActive = share.kind === "active";

  // Resolve the connected channel's display name and owning group for the
  // voice strip. Call channels (`call-<id>`) aren't in the groups tree, so
  // label them generically.
  const { voiceChannelName, voiceGroupId } = (() => {
    if (!voiceChannelId) {
      return { voiceChannelName: "", voiceGroupId: null };
    }
    if (voiceChannelId.startsWith("call-")) {
      return { voiceChannelName: t("profile.call"), voiceGroupId: null };
    }
    for (const group of groupsWithChannels ?? []) {
      const channel = group.channels.find((c) => c.id === voiceChannelId);
      if (channel) {
        return { voiceChannelName: channel.name, voiceGroupId: group.id };
      }
    }
    return { voiceChannelName: t("profile.voice"), voiceGroupId: null };
  })();

  // Route back to the connected room — the same targets terminal's VoiceBar
  // channel-name pill navigates to.
  const returnToVoice = () => {
    if (!voiceChannelId) {
      return;
    }
    if (voiceChannelId.startsWith("call-")) {
      const callId = voiceChannelId.slice("call-".length);
      router.navigate({ to: "/call/$callId", params: { callId } });
    } else if (voiceGroupId) {
      router.navigate({
        to: "/groups/$groupId/voice/$channelId",
        params: { groupId: voiceGroupId, channelId: voiceChannelId },
      });
    }
  };

  const returnToVoiceLabel = voiceChannelId?.startsWith("call-")
    ? tVoice("bar.returnToCall")
    : tVoice("bar.goToChannel", { name: voiceChannelName });

  return (
    <div className="flex shrink-0 flex-col border-t border-line bg-surface">
      {/* Row 1 — identity */}
      <div className="flex items-center gap-2 px-2.5 py-2">
        <Avatar
          avatarKey={profile?.avatar_url}
          size={34}
          presence="online"
          alt={displayName}
          testId="sidebar-profile-avatar"
        />
        <div className="flex min-w-0 flex-1 flex-col leading-tight">
          <span className="truncate text-sm font-semibold text-fg">
            {displayName}
          </span>
          {handle && (
            <bdi className="truncate text-2xs text-muted">{handle}</bdi>
          )}
        </div>
        <button
          type="button"
          data-testid="sidebar-profile-settings"
          onClick={() => router.navigate({ to: "/settings" })}
          aria-label={t("profile.settings")}
          title={t("profile.settings")}
          className="icon-btn-sm"
        >
          <SettingsIcon size={15} className="size-[0.933rem] shrink-0" />
        </button>
      </div>

      {/* Row 2 — persistent voice status (only while connected) */}
      {inVoice && (
        <div
          data-testid="sidebar-voice-strip"
          className="flex items-center gap-1.5 border-t border-line bg-surface-raised px-2.5 py-1 min-h-composer-flush"
        >
          <Volume2
            size={14}
            className="size-[0.933rem] shrink-0"
            style={{ color: "var(--c-voice-connected, var(--c-accent))" }}
          />
          {/* The status text doubles as the way back to the room, mirroring
              Discord's sidebar voice panel and terminal's VoiceBar pill. */}
          <button
            type="button"
            data-testid="sidebar-voice-return"
            onClick={returnToVoice}
            aria-label={returnToVoiceLabel}
            title={returnToVoiceLabel}
            className="group flex min-w-0 flex-1 cursor-pointer flex-col text-start leading-tight"
          >
            <span
              className="text-2xs font-semibold"
              style={{ color: "var(--c-voice-connected, var(--c-accent))" }}
            >
              {t("profile.voiceConnected")}
            </span>
            <span className="truncate text-2xs text-muted transition-colors group-hover:text-fg group-hover:underline">
              {voiceChannelName}
            </span>
          </button>
          {/* Mic (four-state) + deafen. Same information and affordances as
              terminal's VoiceBar, drawn in refined's idiom. */}
          <SidebarVoiceControls />
          <button
            type="button"
            data-testid="sidebar-voice-screenshare"
            onClick={() => toggleScreenShare(share)}
            aria-label={shareActive ? t("profile.stopScreenShare") : t("profile.shareScreen")}
            title={shareActive ? t("profile.stopScreenShare") : t("profile.shareScreen")}
            className="icon-btn-sm"
            style={shareActive ? { color: "var(--c-accent)" } : undefined}
          >
            {shareActive ? <MonitorOff size={15} /> : <Monitor size={15} />}
          </button>
          <button
            type="button"
            data-testid="sidebar-voice-disconnect"
            onClick={() => voiceSession.leave()}
            aria-label={t("profile.disconnect")}
            title={t("profile.disconnect")}
            className="icon-btn-sm hover:text-danger"
          >
            <PhoneOff size={15} />
          </button>
        </div>
      )}
    </div>
  );
});
