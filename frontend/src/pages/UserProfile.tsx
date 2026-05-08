import React from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ArrowLeft, MessageCircle, Ban } from "lucide-react";
import { PageShell } from "../components/Layout/PageShell";
import { PresenceAvatar } from "../components/ui/PresenceAvatar";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { useOtherUserProfile } from "../hooks/queries/useUserProfile";
import { useBlockUser } from "../hooks/queries";
import { useCreateOrGetDMConversation } from "../hooks/queries/useMessages";
import { useAppStore } from "../stores/appStore";

export const UserProfilePage: React.FC = () => {
  const navigate = useNavigate();
  const { userId } = useParams({ from: "/user/$userId" });
  const currentUser = useAppStore((s) => s.currentUser);

  const { data: profile, isLoading } = useOtherUserProfile(userId);
  const blockMutation = useBlockUser();
  const dmMutation = useCreateOrGetDMConversation();

  const isSelf = currentUser?.id === userId;

  const handleBlock = async () => {
    try {
      await blockMutation.mutateAsync(userId);
      navigate({ to: "/dms" });
    } catch (err) {
      console.error("Failed to block user:", err);
    }
  };

  const handleDM = async () => {
    if (!profile?.username) {
      return;
    }
    try {
      const channel = await dmMutation.mutateAsync(profile.username);
      navigate({ to: "/dms/$conversationId", params: { conversationId: channel.id } });
    } catch (err) {
      console.error("Failed to start DM:", err);
    }
  };

  const headlineName =
    profile?.preferred_name || (profile?.username ? `@${profile.username}` : "User");
  const title = profile?.preferred_name || (profile?.username ? `@${profile.username}` : "Profile");

  const items: TerminalMenuItem[] = !profile || isSelf
    ? [
        {
          id: "back",
          label: "Go back",
          icon: <ArrowLeft size={14} />,
          action: () => navigate({ to: "/dms" }),
          type: "system",
          testId: "user-profile-back",
        },
      ]
    : [
        {
          id: "send-message",
          label: "Send Message",
          icon: <MessageCircle size={14} />,
          action: handleDM,
          disabled: dmMutation.isPending,
          testId: "user-profile-dm",
        },
        {
          id: "block",
          label: "Block",
          icon: <Ban size={14} />,
          action: handleBlock,
          disabled: blockMutation.isPending,
          type: "system",
          testId: "user-profile-block",
        },
        { id: "__sep__", label: "", type: "separator" },
        {
          id: "back",
          label: "Go back",
          icon: <ArrowLeft size={14} />,
          action: () => navigate({ to: "/dms" }),
          type: "system",
          testId: "user-profile-back",
        },
      ];

  return (
    <PageShell title={title} scrollable>
      <div data-testid="user-profile-page" className="flex justify-center px-6 py-10">
        <div className="w-full max-w-md flex flex-col gap-6">
          {isLoading ? (
            <span className="text-xs font-mono self-center" style={{ color: "var(--c-text-muted)" }}>
              Loading…
            </span>
          ) : !profile ? (
            <span className="text-xs font-mono self-center" style={{ color: "var(--c-text-muted)" }}>
              User not found
            </span>
          ) : (
            <>
              {/* Header: name on the left, avatar inline on the right.
                  preferred_name takes the headline when set; @username is
                  always shown (as headline if no preferred_name, otherwise
                  as the secondary handle). */}
              <div className="flex items-center justify-between gap-4">
                <div className="flex flex-col min-w-0">
                  <div
                    data-testid="user-profile-headline"
                    className="font-mono text-2xl truncate"
                    style={{ color: "var(--c-accent)" }}
                  >
                    {headlineName}
                  </div>
                  {profile.preferred_name && profile.username && (
                    <div
                      data-testid="user-profile-username"
                      className="font-mono text-xs truncate"
                      style={{ color: "var(--c-text-muted)" }}
                    >
                      @{profile.username}
                    </div>
                  )}
                </div>
                <PresenceAvatar
                  userId={profile.id}
                  avatarKey={profile.avatar_url}
                  size={72}
                  alt={`${headlineName} avatar`}
                  testId="user-profile-avatar"
                  variant="profile"
                />
              </div>

              <div style={{ borderTop: "1px solid var(--c-border)" }}>
                <TerminalMenu items={items} onEsc={() => navigate({ to: "/dms" })} />
              </div>
            </>
          )}
        </div>
      </div>
    </PageShell>
  );
};
