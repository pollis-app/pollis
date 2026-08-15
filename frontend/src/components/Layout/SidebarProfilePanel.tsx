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

  // Resolve the connected channel's display name for the voice strip. Call
  // channels (`call-<id>`) aren't in the groups tree, so label them generically.
  const voiceChannelName = (() => {
    if (!voiceChannelId) {
      return "";
    }
    if (voiceChannelId.startsWith("call-")) {
      return t("profile.call");
    }
    for (const group of groupsWithChannels ?? []) {
      const channel = group.channels.find((c) => c.id === voiceChannelId);
      if (channel) {
        return channel.name;
      }
    }
    return t("profile.voice");
  })();

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
          className="flex items-center gap-1.5 border-t border-line bg-surface-raised px-2.5 py-1.5"
        >
          <Volume2
            size={14}
            className="size-[0.933rem] shrink-0"
            style={{ color: "var(--c-voice-connected, var(--c-accent))" }}
          />
          <div className="flex min-w-0 flex-1 flex-col leading-tight">
            <span
              className="text-2xs font-semibold"
              style={{ color: "var(--c-voice-connected, var(--c-accent))" }}
            >
              {t("profile.voiceConnected")}
            </span>
            <span className="truncate text-2xs text-muted">{voiceChannelName}</span>
          </div>
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
            className="icon-btn-sm hover:text-[var(--c-danger)]"
          >
            <PhoneOff size={15} />
          </button>
        </div>
      )}
    </div>
  );
});
