import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { ShieldCheck, ShieldAlert } from "lucide-react";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import {
  useGroupInvites,
  useGroupMembers,
  useRevokeGroupInvite,
  useSetMemberRole,
  type GroupInvite,
} from "../hooks/queries/useGroups";
import { activeLocale } from "../utils/format";
import { parseServerTimestamp } from "../utils/newMember";
import { usePeerVerificationMap } from "../hooks/queries/useUserProfile";
import { Switch } from "../components/ui/Switch";
import { Button } from "../components/ui/Button";
import { NavigableList } from "../components/ui/NavigableList";
import { useGroupAliases } from "../hooks/queries/useGroupAliases";
import { aliasName, hasAlias } from "../utils/groupAlias";
import { isBotUser } from "../utils/bots";
import { BotBadge } from "../components/ui/BotBadge";

interface MembersProps {
  groupId: string;
  groupName: string;
  isAdmin: boolean;
}

export const Members: React.FC<MembersProps> = observer(({ groupId, groupName, isAdmin }) => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();
  const currentUser = appStore.currentUser;
  const { data: members = [], isLoading } = useGroupMembers(groupId);
  const setRoleMutation = useSetMemberRole();
  const aliases = useGroupAliases(groupId);
  // Same query as the DM sidebar, so badges agree everywhere.
  const verificationByPeer = usePeerVerificationMap();

  return (
    <>
      <NavigableList
        items={members}
        isLoading={isLoading}
        emptyLabel={t("members.empty")}
        testId="members-list"
        getKey={(m) => m.user_id}
        rowTestId={(m) => `member-row-${m.user_id}`}
        activateRowOn="press"
        onClickRow={(m) => {
          const isSelf = m.user_id === currentUser?.id;
          if (isSelf) {
            return;
          }
          navigate({ to: "/groups/$groupId/members/$userId", params: { groupId, userId: m.user_id } });
        }}
        onEnterRow={(m) => {
          const isSelf = m.user_id === currentUser?.id;
          if (isSelf) {
            return;
          }
          navigate({ to: "/groups/$groupId/members/$userId", params: { groupId, userId: m.user_id } });
        }}
        renderRow={(m) => {
          const isSelf = m.user_id === currentUser?.id;
          const verification = verificationByPeer.get(m.user_id);
          const badge = isSelf ? null : verification?.key_changed ? (
            <span
              data-testid={`member-verification-changed-${m.user_id}`}
              title={t("members.keyChanged")}
              style={{ display: "inline-flex", color: "#f0b429", flexShrink: 0 }}
            >
              <ShieldAlert size={14} />
            </span>
          ) : verification?.verified ? (
            <span
              data-testid={`member-verification-verified-${m.user_id}`}
              title={t("members.verified")}
              className="text-accent"
              style={{ display: "inline-flex", flexShrink: 0 }}
            >
              <ShieldCheck size={14} />
            </span>
          ) : null;
          return (
            <span
              className="flex-1 truncate flex items-center gap-2 text-fg"
            >
              <bdi data-testid={`member-name-${m.user_id}`} className="truncate">
                {aliasName(aliases, m.user_id, m.username ?? m.user_id)}
              </bdi>
              {/* The handle beside an alias (#2003), so who is who stays clear. */}
              {hasAlias(aliases, m.user_id) && aliases.get(m.user_id)?.display_name && m.username && (
                <bdi data-testid={`member-handle-${m.user_id}`} className="truncate text-muted">
                  @{m.username}
                </bdi>
              )}
              {isBotUser(m) && (
                <BotBadge groupName={groupName} data-testid={`member-bot-badge-${m.user_id}`} />
              )}
              {badge}
              {isSelf && (
                <span className="ms-1 text-muted">
                  {t("members.self")}
                </span>
              )}
            </span>
          );
        }}
        controls={(m) => {
          const isSelf = m.user_id === currentUser?.id;
          if (!isAdmin || isSelf) {
            return [];
          }
          // A bot is never an admin and leaves through its own page (#1182).
          if (isBotUser(m)) {
            return [
              <Button
                size="sm"
                variant="secondary"
                data-testid={`member-manage-bot-${m.user_id}`}
                onClick={() =>
                  navigate({
                    to: "/groups/$groupId/bots/$botId",
                    params: { groupId, botId: m.user_id },
                  })
                }
              >
                {t("bots.manage")}
              </Button>,
            ];
          }
          return [
            <Switch
              id={`member-admin-toggle-${m.user_id}`}
              label={t("members.adminToggle")}
              checked={m.role === "admin"}
              onChange={() => {
                const newRole = m.role === "admin" ? "member" : "admin";
                setRoleMutation.mutate({ groupId, userId: m.user_id, role: newRole });
              }}
            />,
            ...(hasAlias(aliases, m.user_id)
              ? [
                  <Button
                    size="sm"
                    variant="secondary"
                    data-testid={`member-reset-alias-${m.user_id}`}
                    onClick={() =>
                      navigate({
                        to: "/groups/$groupId/members/$userId/reset-alias",
                        params: { groupId, userId: m.user_id },
                      })
                    }
                  >
                    {t("members.resetAlias")}
                  </Button>,
                ]
              : []),
            <Button size="sm"
              data-testid={`member-kick-${m.user_id}`}
              variant="danger"
              onClick={() =>
                navigate({
                  to: "/groups/$groupId/members/$userId/kick",
                  params: { groupId, userId: m.user_id },
                })
              }
            >
              {t("members.kick")}
            </Button>,
          ];
        }}
        trailing={(m) => {
          const isSelf = m.user_id === currentUser?.id;
          if (isAdmin && !isSelf) {
            return null;
          }
          // `m.role` is a wire value; two literal keys for `scripts/i18n-check.mjs`, raw value as fallback.
          const roleLabel =
            m.role === "admin"
              ? t("members.role.admin")
              : m.role === "member"
                ? t("members.role.member")
                : m.role;
          return <span>{roleLabel}</span>;
        }}
      />
      <InvitedSection groupId={groupId} isAdmin={isAdmin} />
    </>
  );
});


/** Pending invites (#1506); inviter and admins may revoke. Hidden when empty. */
const InvitedSection: React.FC<{ groupId: string; isAdmin: boolean }> = observer(
  ({ groupId, isAdmin }) => {
    const { t } = useTranslation("channels");
    const currentUser = appStore.currentUser;
    const { data: invites = [] } = useGroupInvites(groupId);
    const revoke = useRevokeGroupInvite();

    if (invites.length === 0) {
      return null;
    }

    const invitedWhen = (invite: GroupInvite): string => {
      const ms = parseServerTimestamp(invite.created_at);
      return ms === null ? invite.created_at : new Date(ms).toLocaleDateString(activeLocale());
    };
    const inviteeName = (invite: GroupInvite): string =>
      invite.invitee_username ?? invite.invitee_id;
    const revokingId = revoke.isPending ? revoke.variables?.inviteId : undefined;

    return (
      <section data-testid="invited-section" className="flex-none flex flex-col pt-4">
        <h2 className="px-4 pb-2 text-secondary font-mono font-medium text-dim">
          {t("members.invited.heading")}
        </h2>
        <NavigableList
          items={invites}
          autoFocus={false}
          cursorOnlyWhenFocused
          testId="invited-list"
          getKey={(invite) => invite.id}
          rowTestId={(invite) => `invited-row-${invite.invitee_id}`}
          renderRow={(invite) => (
            <span className="flex-1 min-w-0 flex flex-col gap-0.5">
              <span className="truncate text-fg">{inviteeName(invite)}</span>
              <span className="truncate text-muted">
                {invite.inviter_username
                  ? t("members.invited.by", {
                      name: invite.inviter_username,
                      date: invitedWhen(invite),
                    })
                  : t("members.invited.byUnknown", { date: invitedWhen(invite) })}
              </span>
            </span>
          )}
          controls={(invite) => {
            const mayRevoke = isAdmin || invite.inviter_id === currentUser?.id;
            if (!mayRevoke) {
              return [];
            }
            const busy = revokingId === invite.id;
            return [
              <Button
                size="sm"
                variant="danger"
                data-testid={`invited-revoke-${invite.invitee_id}`}
                aria-label={t("members.invited.revokeLabel", { name: inviteeName(invite) })}
                disabled={revoke.isPending && !busy}
                isLoading={busy}
                loadingText={t("members.invited.revoking")}
                onClick={() => revoke.mutate({ groupId, inviteId: invite.id })}
              >
                {t("members.invited.revoke")}
              </Button>,
            ];
          }}
        />
        {revoke.isError && (
          <p data-testid="invited-revoke-error" className="px-4 pt-2 text-secondary font-mono text-dim">
            {t("members.invited.revokeFailed")}
          </p>
        )}
      </section>
    );
  },
);
