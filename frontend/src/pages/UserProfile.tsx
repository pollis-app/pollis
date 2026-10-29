import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ArrowLeft, MessageCircle, Ban, Flag } from "lucide-react";
import { PageShell } from "../components/Layout/PageShell";
import { PresenceAvatar } from "../components/ui/PresenceAvatar";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { useOtherUserProfile } from "../hooks/queries/useUserProfile";
import { AccountKeyAuditLine } from "../components/Security/AccountKeyAuditLine";
import { useBlockUser, usePeerAuditAccountKey } from "../hooks/queries";
import { useCreateOrGetDMConversation } from "../hooks/queries/useMessages";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";

export const UserProfilePage: React.FC = observer(() => {
  const { t } = useTranslation("dms");
  const navigate = useNavigate();
  const { userId } = useParams({ from: "/user/$userId" });
  const currentUser = appStore.currentUser;

  const { data: profile, isLoading } = useOtherUserProfile(userId);
  const { data: peerAudit } = usePeerAuditAccountKey(userId);
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
    profile?.preferred_name
    || (profile?.username ? `@${profile.username}` : t("profile.fallbackName"));
  const title =
    profile?.preferred_name
    || (profile?.username ? `@${profile.username}` : t("profile.fallbackTitle"));

  const items: TerminalMenuItem[] = !profile || isSelf
    ? [
        {
          id: "back",
          label: t("common:actions.goBack"),
          icon: <ArrowLeft size={14} className="rtl-mirror" />,
          action: () => navigate({ to: "/dms" }),
          type: "system",
          testId: "user-profile-back",
          activateOn: "press",
        },
      ]
    : [
        {
          id: "send-message",
          label: t("profile.sendMessage"),
          icon: <MessageCircle size={14} />,
          action: handleDM,
          disabled: dmMutation.isPending,
          testId: "user-profile-dm",
        },
        {
          id: "block",
          label: t("profile.block"),
          icon: <Ban size={14} />,
          action: handleBlock,
          disabled: blockMutation.isPending,
          type: "system",
          testId: "user-profile-block",
        },
        {
          id: "report",
          label: t("profile.report"),
          icon: <Flag size={14} />,
          action: () => navigate({ to: "/report/$userId", params: { userId } }),
          type: "system",
          testId: "user-profile-report",
        },
        { id: "__sep__", label: "", type: "separator" },
        {
          id: "back",
          label: t("common:actions.goBack"),
          icon: <ArrowLeft size={14} className="rtl-mirror" />,
          action: () => navigate({ to: "/dms" }),
          type: "system",
          testId: "user-profile-back",
          activateOn: "press",
        },
      ];

  return (
    <PageShell title={title} scrollable>
      <div data-testid="user-profile-page" className="flex justify-center px-6 py-10">
        <div className="w-full max-w-md flex flex-col gap-6">
          {isLoading ? (
            <span className="text-xs font-mono self-center text-muted">
              {t("common:states.loading")}
            </span>
          ) : !profile ? (
            <span className="text-xs font-mono self-center text-muted">
              {t("profile.notFound")}
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
                    className="font-mono text-2xl truncate text-accent"
                  >
                    {headlineName}
                  </div>
                  {profile.preferred_name && profile.username && (
                    <div
                      data-testid="user-profile-username"
                      className="font-mono text-xs truncate text-muted"
                    >
                      <bdi>@{profile.username}</bdi>
                    </div>
                  )}
                </div>
                <PresenceAvatar
                  userId={profile.id}
                  avatarKey={profile.avatar_url}
                  size={72}
                  alt={t("profile.avatarAlt", { name: headlineName })}
                  testId="user-profile-avatar"
                  variant="profile"
                />
              </div>

              {/* Account-key transparency: advisory audit of this peer's
                  published identity key against the public log (#330). */}
              {!isSelf && peerAudit && (
                <AccountKeyAuditLine
                  status={peerAudit.status}
                  detail={peerAudit.detail}
                  testId="peer-account-key-audit"
                />
              )}

              <div className="border-t border-line">
                <TerminalMenu items={items} onEsc={() => navigate({ to: "/dms" })} />
              </div>
            </>
          )}
        </div>
      </div>
    </PageShell>
  );
});
