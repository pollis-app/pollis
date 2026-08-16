import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { Palette, User, ShieldCheck, Volume2, Keyboard } from "lucide-react";
import { PageShell } from "../components/Layout/PageShell";
import { PresenceAvatar } from "../components/ui/PresenceAvatar";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { useUserProfile } from "../hooks/queries";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";

export const SettingsHubPage: React.FC = observer(() => {
  const { t } = useTranslation("settings");
  const navigate = useNavigate();
  const currentUser = appStore.currentUser;
  const { data: profile } = useUserProfile();

  const headlineName =
    profile?.preferred_name ||
    (profile?.username ? `@${profile.username}` : t("hub.headlineFallback"));

  const items: TerminalMenuItem[] = [
    {
      id: "preferences",
      label: t("hub.preferencesLabel"),
      icon: <Palette size={14} />,
      description: t("hub.preferencesDescription"),
      action: () => navigate({ to: "/preferences" }),
      testId: "menu-item-preferences",
    },
    {
      id: "user",
      label: t("hub.userLabel"),
      icon: <User size={14} />,
      description: t("hub.userDescription"),
      action: () => navigate({ to: "/user" }),
      testId: "menu-item-user",
    },
    {
      id: "voice",
      label: t("hub.voiceLabel"),
      icon: <Volume2 size={14} />,
      description: t("hub.voiceDescription"),
      action: () => navigate({ to: "/voice-settings" }),
      testId: "menu-item-voice-settings",
    },
    {
      id: "security",
      label: t("hub.securityLabel"),
      icon: <ShieldCheck size={14} />,
      description: t("hub.securityDescription"),
      action: () => navigate({ to: "/security" }),
      testId: "menu-item-security",
    },
    {
      id: "shortcuts",
      label: t("hub.shortcutsLabel"),
      icon: <Keyboard size={14} />,
      description: t("hub.shortcutsDescription"),
      action: () => navigate({ to: "/shortcuts" }),
      testId: "menu-item-shortcuts",
    },
  ];

  return (
    <PageShell title={t("hub.title")} scrollable>
      <div data-testid="settings-hub-page" className="flex justify-center px-6 py-10">
        <div className="w-full max-w-md flex flex-col gap-6">
          {/* Own-profile header — mirrors the layout of viewing another
              user's profile (name on the left, avatar on the right). */}
          <div className="flex items-center justify-between gap-4">
            <div className="flex flex-col min-w-0">
              <div
                data-testid="settings-hub-headline"
                className="font-mono text-2xl truncate text-accent"
              >
                {headlineName}
              </div>
              {profile?.preferred_name && profile?.username && (
                <div
                  data-testid="settings-hub-username"
                  className="font-mono text-xs truncate text-muted"
                >
                  <bdi>@{profile.username}</bdi>
                </div>
              )}
            </div>
            <PresenceAvatar
              userId={currentUser?.id}
              avatarKey={profile?.avatar_url}
              size={72}
              alt={t("hub.avatarAlt", { name: headlineName })}
              testId="settings-hub-avatar"
              variant="profile"
            />
          </div>

          <div className="border-t border-line">
            <TerminalMenu items={items} onEsc={() => navigate({ to: "/" })} />
          </div>
        </div>
      </div>
    </PageShell>
  );
});
