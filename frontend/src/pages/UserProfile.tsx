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
import { useGroupAliases } from "../hooks/queries/useGroupAliases";

export const UserProfilePage: React.FC = observer(() => {
  const { t } = useTranslation("dms");
  const navigate = useNavigate();
  // `/user/$userId`, or `/groups/$groupId/members/$userId` for the group's view of them (#2003).
  const { userId, groupId } = useParams({ strict: false }) as { userId: string; groupId?: string };
  const alias = useGroupAliases(groupId ?? null).get(userId);
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

  const shownName = alias?.display_name || profile?.preferred_name;
  const headlineName =
    shownName
    || (profile?.username ? `@${profile.username}` : t("profile.fallbackName"));
  const title =
    shownName
    || (profile?.username ? `@${profile.username}` : t("profile.fallbackTitle"));
  const back = () =>
    groupId
      ? navigate({ to: "/groups/$groupId/members", params: { groupId } })
      : navigate({ to: "/dms" });

  const backItem: TerminalMenuItem = {
    id: "back",
    label: t("common:actions.goBack"),
    icon: <ArrowLeft size={14} className="rtl-mirror" />,
    action: back,
    type: "system",
    testId: "user-profile-back",
    activateOn: "press",
  };

  const items: TerminalMenuItem[] = !profile || isSelf
    ? [
        backItem,
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
        backItem,
      ];

  return (
    <PageShell title={title} scrollable>
      <div data-testid="user-profile-page" className="flex justify-center px-6 py-10">
        <div className="w-full max-w-md flex flex-col gap-6">
          {isLoading ? (
            <span className="text-secondary font-mono self-center text-muted">
              {t("common:states.loading")}
            </span>
          ) : !profile ? (
            <span className="text-secondary font-mono self-center text-muted">
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
                    className="font-mono text-display truncate text-accent"
                  >
                    {headlineName}
                  </div>
                  {shownName && profile.username && (
                    <div
                      data-testid="user-profile-username"
                      className="font-mono text-secondary truncate text-muted"
                    >
                      <bdi>@{profile.username}</bdi>
                    </div>
                  )}
                </div>
                <PresenceAvatar
                  userId={profile.id}
                  avatarKey={alias?.avatar_url || profile.avatar_url}
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
                <TerminalMenu items={items} onEsc={back} />
              </div>
            </>
          )}
        </div>
      </div>
    </PageShell>
  );
});
