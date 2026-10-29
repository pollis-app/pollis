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

interface MembersProps {
  groupId: string;
  isAdmin: boolean;
}

export const Members: React.FC<MembersProps> = observer(({ groupId, isAdmin }) => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();
  const currentUser = appStore.currentUser;
  const { data: members = [], isLoading } = useGroupMembers(groupId);
  const setRoleMutation = useSetMemberRole();
  // peerUserId → { verified, key_changed }. Reuses the same query the DM
  // sidebar already loads, so the badge state is consistent across every
  // surface where the same person appears (DM, group, channel author).
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
          navigate({ to: "/user/$userId", params: { userId: m.user_id } });
        }}
        onEnterRow={(m) => {
          const isSelf = m.user_id === currentUser?.id;
          if (isSelf) {
            return;
          }
          navigate({ to: "/user/$userId", params: { userId: m.user_id } });
        }}
        renderRow={(m) => {
          const isSelf = m.user_id === currentUser?.id;
          // Verified badge wins; a `key_changed` mismatch overrides it
          // (matches the DM sidebar logic — see Sidebar.tsx).
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
              <span className="truncate">{m.username ?? m.user_id}</span>
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
            <Button size="sm"
              data-testid={`member-kick-${m.user_id}`}
              variant="primary"
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
          // `m.role` is the wire value ("admin" / "member") — a protocol token,
          // not copy. It happens to read as English, which is exactly why it
          // shipped straight to screen untranslated.
          //
          // Written as two literal keys rather than t(`members.role.${m.role}`)
          // so `scripts/i18n-check.mjs` can see them: its call-site scan only
          // resolves literals, and a computed key is invisible to it. A role the
          // client does not know falls back to the raw value, which is still
          // better than a blank slot.
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


/**
 * Pending invites (#1506): who was invited, by whom, and when. Every member
 * sees it; the member who sent an invite and the group's admins can revoke it.
 * Hidden while there are none — an empty "Invited" heading says nothing.
 */
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
        <h2 className="px-4 pb-2 text-xs font-mono font-medium text-dim">
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
                variant="primary"
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
          <p data-testid="invited-revoke-error" className="px-4 pt-2 text-xs font-mono text-dim">
            {t("members.invited.revokeFailed")}
          </p>
        )}
      </section>
    );
  },
);
