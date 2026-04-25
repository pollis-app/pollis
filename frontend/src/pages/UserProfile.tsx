import React from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { MessageCircle, Ban } from "lucide-react";
import { PageShell } from "../components/Layout/PageShell";
import { Avatar } from "../components/ui/Avatar";
import { Button } from "../components/ui/Button";
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

  const title = profile?.username ? `@${profile.username}` : "Profile";

  return (
    <PageShell title={title} scrollable>
      <div
        data-testid="user-profile-page"
        className="flex flex-col items-center gap-6 px-6 py-10"
      >
        {isLoading ? (
          <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
            Loading…
          </span>
        ) : !profile ? (
          <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
            User not found
          </span>
        ) : (
          <>
            <Avatar
              avatarKey={profile.avatar_url}
              size={96}
              alt={`${profile.username} avatar`}
              testId="user-profile-avatar"
              variant="profile"
            />
            <div
              data-testid="user-profile-username"
              className="font-mono text-lg"
              style={{ color: "var(--c-text)" }}
            >
              @{profile.username}
            </div>

            {!isSelf && (
              <div className="flex items-center gap-3">
                <Button
                  data-testid="user-profile-dm"
                  onClick={handleDM}
                  disabled={dmMutation.isPending}
                  aria-label="Send direct message"
                >
                  <MessageCircle size={14} />
                  <span>Send Message</span>
                </Button>
                <Button
                  data-testid="user-profile-block"
                  onClick={handleBlock}
                  disabled={blockMutation.isPending}
                  variant="secondary"
                  aria-label="Block user"
                >
                  <Ban size={14} />
                  <span>Block</span>
                </Button>
              </div>
            )}
          </>
        )}
      </div>
    </PageShell>
  );
};
