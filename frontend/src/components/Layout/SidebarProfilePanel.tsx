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
import { PressButton } from "../ui/PressButton";

/**
 * Discord-style identity panel at the sidebar bottom (refined skin). While in
 * voice it also carries the full persistent voice strip, because refined hides
 * terminal's `VoiceBar`.
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

  // Call channels (`call-<id>`) aren't in the groups tree, so they get a
  // generic label.
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

  // Same targets as terminal's VoiceBar channel-name pill.
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
        <PressButton
          type="button"
          data-testid="sidebar-profile-settings"
          onClick={() => router.navigate({ to: "/settings" })}
          aria-label={t("profile.settings")}
          title={t("profile.settings")}
          className="icon-btn-sm"
        >
          <SettingsIcon size={15} className="size-[0.933rem] shrink-0" />
        </PressButton>
      </div>

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
          <PressButton
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
          </PressButton>
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
