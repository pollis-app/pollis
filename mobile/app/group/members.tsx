import { useMemo, useState } from "react";
import { View } from "react-native";
import { useNav, useRouteParams } from "../../components/pane/paneContext";
import { useTranslation } from "react-i18next";
import { Screen, Header, Body, ListRow, Group, Chip, SectionTitle } from "../../components/ui";
import { UserAvatar } from "../../components/UserAvatar";
import { BotBadge } from "../../components/BotBadge";
import { AppMark } from "../../components/bots/AppMark";
import { isBotUser } from "../../lib/bots";
import { Hint, ErrorText } from "../../components/groups/FormBits";
import { space } from "../../theme/tokens";
import {
  useGroupInvites,
  useGroupMembers,
  useRevokeGroupInvite,
  useRemoveMember,
  useSetMemberRole,
  useUserGroupsWithChannels,
  useUserProfiles,
  useGroupApps,
  sortMembersByRole,
} from "../../hooks/queries";
import { personName } from "../../lib/personName";
import { aliasAvatar, hasAlias } from "../../lib/groupAlias";
import { useGroupAliases, useResetGroupAlias } from "../../hooks/queries/useGroupAliases";
import { activeLocale } from "../../i18n";
import { parseServerTimestamp } from "../../lib/newMember";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";
import { haptic } from "../../lib/haptics";
import { useThemeRerender } from "../../components/theme";

function Members() {
  useThemeRerender();
  const { t } = useTranslation("channels");
  const router = useNav();
  const { groupId } = useRouteParams<{ groupId?: string }>();
  const id = groupId ?? null;
  const currentUser = appStore.currentUser;

  const { data: members = [], isLoading } = useGroupMembers(id);
  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === id);
  const groupName = group?.name ?? t("mobile:group.common.fallbackName");
  const removeMember = useRemoveMember(id);
  // Bots running a development build carry a mark beside their badge for every member (#2005).
  const { data: apps = [] } = useGroupApps(id);
  const devBots = useMemo(() => new Set(apps.filter((a) => a.development).map((a) => a.botUserId)), [apps]);
  const setRole = useSetMemberRole(id);
  // Pending invites (#1506); the inviter and admins can revoke.
  const { data: invites = [] } = useGroupInvites(id);
  const revokeInvite = useRevokeGroupInvite(id);
  const revokingId = revokeInvite.isPending ? revokeInvite.variables : undefined;
  const invitedWhen = (createdAt: string) => {
    const ms = parseServerTimestamp(createdAt);
    return ms === null ? createdAt : new Date(ms).toLocaleDateString(activeLocale());
  };

  // Role then alphabetical (no presence source on mobile).
  const sortedMembers = useMemo(() => sortMembersByRole(members), [members]);
  // Display names (#1971): one batched read; rows show the handle until it lands.
  const personIds = useMemo(
    () => [...members.map((m) => m.user_id), ...invites.map((inv) => inv.invitee_id)],
    [members, invites],
  );
  const { data: profiles } = useUserProfiles(personIds);
  // Per-group aliases (#2003): the alias headlines, the handle stays beside it.
  const aliases = useGroupAliases(id);
  const resetAlias = useResetGroupAlias();
  const [confirmReset, setConfirmReset] = useState<string | null>(null);
  const onResetAlias = (memberId: string) => {
    if (!id) {
      return;
    }
    if (confirmReset !== memberId) {
      haptic("confirmArm");
      setConfirmReset(memberId);
      return;
    }
    resetAlias.mutate({ groupId: id, userId: memberId }, { onSettled: () => setConfirmReset(null) });
  };

  const myRole = useMemo(
    () => members.find((m) => m.user_id === currentUser?.id)?.role,
    [members, currentUser?.id],
  );
  const iAmAdmin = myRole === "admin" || myRole === "owner";

  const [confirmRemove, setConfirmRemove] = useState<string | null>(null);

  const onRemove = (memberId: string) => {
    if (confirmRemove !== memberId) {
      haptic("confirmArm");
      setConfirmRemove(memberId);
      return;
    }
    removeMember.mutate(memberId, {
      onSettled: () => setConfirmRemove(null),
    });
  };

  const onToggleRole = (memberId: string, role: string) => {
    setRole.mutate({
      userId: memberId,
      role: role === "admin" ? "member" : "admin",
    });
  };

  return (
    <Screen testID="screen-group-members" aboveTabBar={router.inPane}>
      <Header onBack={router.onBack}
        title={t("mobile:group.panel.members")}
        subtitle={
          group
            ? `${group.name} · ${t("mobile:group.detail.memberCount", { count: members.length })}`
            : t("mobile:group.detail.memberCount", { count: members.length })
        }
      />
      <Body contentContainerStyle={{ padding: space.xxl, gap: space.lg }}>
        {isLoading ? <Hint>{t("common:states.loading")}</Hint> : null}
        {sortedMembers.length > 0 ? (
          <Group>
            {sortedMembers.map((m) => {
              const isMe = m.user_id === currentUser?.id;
              const isOwner = m.role === "owner";
              const isAdmin = m.role === "admin";
              const armed = confirmRemove === m.user_id;
              const aliasDisplay = aliases.get(m.user_id)?.display_name;
              const label = aliasDisplay
                ? { name: aliasDisplay, isHandle: false }
                : personName(
                    { username: m.username, preferred_name: profiles?.get(m.user_id)?.preferred_name },
                    m.user_id.slice(0, 8),
                  );
              const handle = label.name;
              const aliased = hasAlias(aliases, m.user_id);
              const resetArmed = confirmReset === m.user_id;
              const name = isMe ? `${handle} ${t("members.self")}` : handle;
              // A bot (#1182): badged, managed by the group's admins, never given a role.
              const isBot = isBotUser(m);
              const managedBy = t("mobile:bots.managedBy", { group: groupName });
              const role = isOwner
                ? t("mobile:group.members.roleOwner")
                : isAdmin
                  ? t("mobile:group.members.roleAdmin")
                  : t("mobile:group.members.joined", {
                      date: new Date(m.joined_at).toLocaleDateString(activeLocale()),
                    });
              return (
                <View key={m.user_id}>
                  <ListRow
                    testID={`row-member-${m.user_id}`}
                    minHeight={64}
                    glyph={
                      <UserAvatar
                        testID={`avatar-member-${m.user_id}`}
                        userId={m.user_id}
                        avatarKey={m.avatar_url}
                        aliasKey={aliasAvatar(aliases, m.user_id, null)}
                        label={m.username || m.user_id}
                        variant={isMe ? "self" : "default"}
                        on="raised"
                      />
                    }
                    name={name}
                    nameIsHandle={label.isHandle}
                    sub={
                      isBot
                        ? managedBy
                        : aliasDisplay && m.username
                          ? `@${m.username} · ${role}`
                          : role
                    }
                    end={
                      isBot ? (
                        <>
                          <BotBadge testID={`badge-bot-${m.user_id}`} />
                          {devBots.has(m.user_id) ? (
                            <AppMark kind="development" testID={`mark-bot-dev-${m.user_id}`} />
                          ) : null}
                        </>
                      ) : undefined
                    }
                    accessibilityLabel={
                      isBot
                        ? devBots.has(m.user_id)
                          ? `${name}, ${t("mobile:bots.badgeA11y", { group: groupName })}, ${t("mobile:bots.app.devMark")}`
                          : `${name}, ${t("mobile:bots.badgeA11y", { group: groupName })}`
                        : undefined
                    }
                    chevron={!isMe}
                    onPress={
                      isMe
                        ? undefined
                        : isBot && iAmAdmin && id
                          ? () =>
                              router.push({
                                pathname: "/group/bot",
                                params: { groupId: id, botId: m.user_id },
                              })
                          : () =>
                              router.push({
                                pathname: "/user/[id]",
                                params: id ? { id: m.user_id, groupId: id } : { id: m.user_id },
                              })
                    }
                  />
                  {/* Under the member, so the row stays one control. A bot is managed on its own page. */}
                  {iAmAdmin && !isMe && !isOwner && !isBot ? (
                    <View
                      style={{
                        flexDirection: "row",
                        flexWrap: "wrap",
                        gap: space.sm,
                        paddingStart: 70,
                        paddingEnd: space.xl,
                        paddingBottom: space.sm,
                      }}
                    >
                      <Chip
                        variant="outline"
                        selected={isAdmin}
                        testID={`btn-toggle-role-${m.user_id}`}
                        accessibilityLabel={`${
                          isAdmin
                            ? t("mobile:group.members.removeAdmin")
                            : t("mobile:group.members.makeAdmin")
                        }, ${handle}`}
                        onPress={() => onToggleRole(m.user_id, m.role)}
                      >
                        {setRole.isPending
                          ? "…"
                          : isAdmin
                            ? t("mobile:group.members.removeAdmin")
                            : t("mobile:group.members.makeAdmin")}
                      </Chip>
                      {aliased ? (
                        <Chip
                          variant="outline"
                          selected={resetArmed}
                          testID={`btn-reset-alias-${m.user_id}`}
                          accessibilityLabel={
                            resetArmed
                              ? t("mobile:group.settings.tapAgainToConfirm")
                              : `${t("members.resetAlias")}, ${handle}`
                          }
                          onPress={() => onResetAlias(m.user_id)}
                        >
                          {resetAlias.isPending && resetArmed
                            ? "…"
                            : resetArmed
                              ? t("mobile:group.common.confirm")
                              : t("members.resetAlias")}
                        </Chip>
                      ) : null}
                      <Chip
                        variant="danger"
                        selected={armed}
                        testID={`btn-remove-member-${m.user_id}`}
                        accessibilityLabel={
                          armed
                            ? t("mobile:group.settings.tapAgainToConfirm")
                            : t("mobile:group.members.removeLabel", { name: handle })
                        }
                        onPress={() => onRemove(m.user_id)}
                      >
                        {removeMember.isPending && armed
                          ? "…"
                          : armed
                            ? t("mobile:group.common.confirm")
                            : t("mobile:group.common.remove")}
                      </Chip>
                    </View>
                  ) : null}
                </View>
              );
            })}
          </Group>
        ) : null}
        {invites.length > 0 ? (
          <View testID="section-invited">
            <SectionTitle style={{ paddingHorizontal: 0, paddingTop: space.md }}>
              {t("members.invited.heading")}
            </SectionTitle>
            <Group>
              {invites.map((inv) => {
                const label = personName(
                  {
                    username: inv.invitee_username,
                    preferred_name: profiles?.get(inv.invitee_id)?.preferred_name,
                  },
                  inv.invitee_id.slice(0, 8),
                );
                const handle = label.name;
                const mayRevoke = iAmAdmin || inv.inviter_id === currentUser?.id;
                const busy = revokingId === inv.id;
                const when = invitedWhen(inv.created_at);
                return (
                  <View key={inv.id}>
                    <ListRow
                      testID={`row-invited-${inv.invitee_id}`}
                      minHeight={64}
                      glyph={<UserAvatar userId={inv.invitee_id} label={inv.invitee_username || inv.invitee_id} on="raised" />}
                      name={handle}
                      nameIsHandle={label.isHandle}
                      sub={
                        inv.inviter_username
                          ? t("members.invited.by", { name: `@${inv.inviter_username}`, date: when })
                          : t("members.invited.byUnknown", { date: when })
                      }
                      chevron={false}
                    />
                    {mayRevoke ? (
                      <View
                        style={{
                          flexDirection: "row",
                          paddingStart: 70,
                          paddingEnd: space.xl,
                          paddingBottom: space.sm,
                        }}
                      >
                        <Chip
                          variant="danger"
                          testID={`btn-revoke-invite-${inv.invitee_id}`}
                          accessibilityLabel={t("members.invited.revokeLabel", { name: handle })}
                          disabled={revokeInvite.isPending && !busy}
                          onPress={busy ? undefined : () => revokeInvite.mutate(inv.id)}
                        >
                          {busy ? t("members.invited.revoking") : t("members.invited.revoke")}
                        </Chip>
                      </View>
                    ) : null}
                  </View>
                );
              })}
            </Group>
          </View>
        ) : null}
        {revokeInvite.isError ? (
          <ErrorText>{t("members.invited.revokeFailed")}</ErrorText>
        ) : null}
        {removeMember.isError ? (
          <ErrorText>
            {(removeMember.error as Error).message || t("kickMember.removeFailed")}
          </ErrorText>
        ) : null}
      </Body>
    </Screen>
  );
}

export default observer(Members);
