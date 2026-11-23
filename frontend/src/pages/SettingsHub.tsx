import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { Bookmark, Globe, Palette, User, ShieldCheck, Volume2, Keyboard, Stethoscope, Scale } from "lucide-react";
import { languageOption } from "../i18n/languages";
import { PageShell } from "../components/Layout/PageShell";
import { PresenceAvatar } from "../components/ui/PresenceAvatar";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { useUserProfile } from "../hooks/queries";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";

export const SettingsHubPage: React.FC = observer(() => {
  const { t, i18n } = useTranslation("settings");
  const navigate = useNavigate();
  const currentUser = appStore.currentUser;
  const { data: profile } = useUserProfile();

  const headlineName =
    profile?.preferred_name ||
    (profile?.username ? `@${profile.username}` : t("nav:sidebar.account"));

  // Same items, labels and order as the sidebar's Account section and mobile's
  // Account tab (#2086); desktop-only pages (voice, key bindings) sit where
  // mobile has nothing.
  const items: TerminalMenuItem[] = [
    {
      id: "saved",
      label: t("nav:sidebar.saved"),
      icon: <Bookmark size={14} />,
      description: t("hub.savedDescription"),
      action: () => navigate({ to: "/saved" }),
      testId: "menu-item-saved",
      activateOn: "press",
    },
    {
      id: "user",
      label: t("nav:sidebar.profile"),
      icon: <User size={14} />,
      description: t("hub.userDescription"),
      action: () => navigate({ to: "/user" }),
      testId: "menu-item-user",
      activateOn: "press",
    },
    {
      id: "security",
      label: t("nav:sidebar.security"),
      icon: <ShieldCheck size={14} />,
      description: t("hub.securityDescription"),
      action: () => navigate({ to: "/security" }),
      testId: "menu-item-security",
      activateOn: "press",
    },
    {
      id: "preferences",
      label: t("nav:sidebar.preferences"),
      icon: <Palette size={14} />,
      description: t("hub.preferencesDescription"),
      action: () => navigate({ to: "/preferences" }),
      testId: "menu-item-preferences",
      activateOn: "press",
    },
    {
      id: "language",
      label: t("nav:sidebar.language"),
      icon: <Globe size={14} />,
      description: languageOption(i18n.language)?.label,
      action: () => navigate({ to: "/preferences/language" }),
      testId: "menu-item-language",
      activateOn: "press",
    },
    {
      id: "voice",
      label: t("nav:sidebar.voiceAndVideo"),
      icon: <Volume2 size={14} />,
      description: t("hub.voiceDescription"),
      action: () => navigate({ to: "/voice-settings" }),
      testId: "menu-item-voice-settings",
      activateOn: "press",
    },
    {
      id: "shortcuts",
      label: t("nav:sidebar.keyBindings"),
      icon: <Keyboard size={14} />,
      description: t("hub.shortcutsDescription"),
      action: () => navigate({ to: "/shortcuts" }),
      testId: "menu-item-shortcuts",
      activateOn: "press",
    },
    {
      id: "diagnostics",
      label: t("diagnostics.title"),
      icon: <Stethoscope size={14} />,
      description: t("hub.diagnosticsDescription"),
      action: () => navigate({ to: "/diagnostics" }),
      testId: "menu-item-diagnostics",
      activateOn: "press",
    },
    {
      id: "open-source",
      label: t("openSource.title"),
      icon: <Scale size={14} />,
      description: t("hub.openSourceDescription"),
      action: () => navigate({ to: "/open-source" }),
      testId: "menu-item-open-source",
      activateOn: "press",
    },
  ];

  return (
    <PageShell title={t("nav:sidebar.account")} scrollable>
      <div data-testid="settings-hub-page" className="flex justify-center px-6 py-10">
        <div className="w-full max-w-md flex flex-col gap-6">
          {/* Own-profile header — mirrors the layout of viewing another
              user's profile (name on the left, avatar on the right). */}
          <div className="flex items-center justify-between gap-4">
            <div className="flex flex-col min-w-0">
              <div
                data-testid="settings-hub-headline"
                className="font-mono text-display truncate text-accent"
              >
                {headlineName}
              </div>
              {profile?.preferred_name && profile?.username && (
                <div
                  data-testid="settings-hub-username"
                  className="font-mono text-secondary truncate text-muted"
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
