import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { Palette, User, ShieldCheck, Volume2, Keyboard } from "lucide-react";
import { PageShell } from "../components/Layout/PageShell";
import { PresenceAvatar } from "../components/ui/PresenceAvatar";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { useUserProfile } from "../hooks/queries";
import { useAppStore } from "../stores/appStore";

export const SettingsHubPage: React.FC = () => {
  const navigate = useNavigate();
  const currentUser = useAppStore((s) => s.currentUser);
  const { data: profile } = useUserProfile();

  const headlineName =
    profile?.preferred_name || (profile?.username ? `@${profile.username}` : "Account");

  const items: TerminalMenuItem[] = [
    {
      id: "preferences",
      label: "Preferences",
      icon: <Palette size={14} />,
      description: "Colors, font size, etc.",
      action: () => navigate({ to: "/preferences" }),
      testId: "menu-item-preferences",
    },
    {
      id: "user",
      label: "User Settings",
      icon: <User size={14} />,
      description: "Profile, username, avatar",
      action: () => navigate({ to: "/user" }),
      testId: "menu-item-user",
    },
    {
      id: "voice",
      label: "Voice",
      icon: <Volume2 size={14} />,
      description: "Microphone, speaker, audio processing",
      action: () => navigate({ to: "/voice-settings" }),
      testId: "menu-item-voice-settings",
    },
    {
      id: "security",
      label: "Security",
      icon: <ShieldCheck size={14} />,
      description: "Device enrollments, identity resets",
      action: () => navigate({ to: "/security" }),
      testId: "menu-item-security",
    },
    {
      id: "shortcuts",
      label: "Key Bindings",
      icon: <Keyboard size={14} />,
      description: "Global keyboard shortcuts reference",
      action: () => navigate({ to: "/shortcuts" }),
      testId: "menu-item-shortcuts",
    },
  ];

  return (
    <PageShell title="Account" scrollable>
      <div data-testid="settings-hub-page" className="flex justify-center px-6 py-10">
        <div className="w-full max-w-md flex flex-col gap-6">
          {/* Own-profile header — mirrors the layout of viewing another
              user's profile (name on the left, avatar on the right). */}
          <div className="flex items-center justify-between gap-4">
            <div className="flex flex-col min-w-0">
              <div
                data-testid="settings-hub-headline"
                className="font-mono text-2xl truncate"
                style={{ color: "var(--c-accent)" }}
              >
                {headlineName}
              </div>
              {profile?.preferred_name && profile?.username && (
                <div
                  data-testid="settings-hub-username"
                  className="font-mono text-xs truncate"
                  style={{ color: "var(--c-text-muted)" }}
                >
                  @{profile.username}
                </div>
              )}
            </div>
            <PresenceAvatar
              userId={currentUser?.id}
              avatarKey={profile?.avatar_url}
              size={72}
              alt={`${headlineName} avatar`}
              testId="settings-hub-avatar"
              variant="profile"
            />
          </div>

          <div style={{ borderTop: "1px solid var(--c-border)" }}>
            <TerminalMenu items={items} onEsc={() => navigate({ to: "/" })} />
          </div>
        </div>
      </div>
    </PageShell>
  );
};
