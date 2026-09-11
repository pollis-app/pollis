import { useMemo, useState } from "react";
import { View, Text } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Avatar,
  Chip,
  Ctx,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import {
  useGroupMembers,
  useRemoveMember,
  useSetMemberRole,
  useUserGroupsWithChannels,
  sortMembersByRole,
} from "../../hooks/queries";
import { activeLocale, upper } from "../../i18n";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

function Members() {
  const { t } = useTranslation("channels");
  const router = useRouter();
  const { groupId } = useLocalSearchParams<{ groupId?: string }>();
  const id = groupId ?? null;
  const currentUser = appStore.currentUser;

  const { data: members = [], isLoading } = useGroupMembers(id);
  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === id);
  const removeMember = useRemoveMember(id);
  const setRole = useSetMemberRole(id);

  // Role-then-alphabetical, mirroring desktop's members panel intent —
  // desktop orders online-first, but mobile has no presence source yet, so
  // presence ordering awaits one.
  const sortedMembers = useMemo(() => sortMembersByRole(members), [members]);

  const myRole = useMemo(
    () => members.find((m) => m.user_id === currentUser?.id)?.role,
    [members, currentUser?.id],
  );
  const iAmAdmin = myRole === "admin" || myRole === "owner";

  const [confirmRemove, setConfirmRemove] = useState<string | null>(null);

  const onRemove = (memberId: string) => {
    if (confirmRemove !== memberId) {
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
    <Screen testID="screen-group-members">
      <Crumb
        segs={[
          { label: upper(t("nav:breadcrumb.groups")) },
          { label: group?.name ?? t("mobile:group.common.fallbackName") },
          { label: t("group.members"), leaf: true },
        ]}
        end={String(members.length || 0)}
      />
      <Body>
        <SectionTitle>{upper(t("group.members"))}</SectionTitle>
        {isLoading ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingVertical: 12,
            }}
          >
            {t("common:states.loading")}
          </Text>
        ) : null}
        {sortedMembers.map((m) => {
          const isMe = m.user_id === currentUser?.id;
          const isOwner = m.role === "owner";
          const isAdmin = m.role === "admin";
          const armed = confirmRemove === m.user_id;
          return (
            <ListRow
              key={m.user_id}
              testID={`row-member-${m.user_id}`}
              minHeight={54}
              glyph={
                <Avatar label={(m.username || m.user_id).slice(0, 2)} />
              }
              name={
                <Text
                  style={{
                    fontFamily: ty.rowN.fontFamily,
                    fontSize: 14,
                    color: semantic.ink,
                  }}
                >
                  @{m.username ?? m.user_id.slice(0, 8)}
                  {isMe ? ` ${t("members.self")}` : ""}
                </Text>
              }
              sub={
                isOwner
                  ? t("mobile:group.members.roleOwner")
                  : isAdmin
                    ? t("members.role.admin")
                    : t("mobile:group.members.joined", {
                        date: new Date(m.joined_at).toLocaleDateString(activeLocale()),
                      })
              }
              onPress={
                isMe
                  ? undefined
                  : () =>
                      router.push({
                        pathname: "/user/[id]",
                        params: { id: m.user_id },
                      })
              }
              end={
                iAmAdmin && !isMe && !isOwner ? (
                  <View style={{ flexDirection: "row", gap: 6 }}>
                    <Chip
                      variant={isAdmin ? "on" : "default"}
                      testID={`btn-toggle-role-${m.user_id}`}
                      accessibilityLabel={
                        isAdmin
                          ? t("mobile:group.members.removeAdmin")
                          : t("mobile:group.members.makeAdmin")
                      }
                      onPress={() => onToggleRole(m.user_id, m.role)}
                    >
                      {setRole.isPending
                        ? "…"
                        : isAdmin
                          ? t("members.adminToggle")
                          : t("mobile:group.members.makeAdmin")}
                    </Chip>
                    <Chip
                      variant={armed ? "on" : "default"}
                      testID={`btn-remove-member-${m.user_id}`}
                      accessibilityLabel={t("kickMember.pageTitle")}
                      onPress={() => onRemove(m.user_id)}
                    >
                      {removeMember.isPending && armed
                        ? "…"
                        : armed
                          ? t("mobile:group.common.confirm")
                          : t("mobile:group.common.remove")}
                    </Chip>
                  </View>
                ) : null
              }
            />
          );
        })}
        {removeMember.isError ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.danger,
              paddingHorizontal: 18,
              paddingTop: 6,
            }}
          >
            {(removeMember.error as Error).message || t("kickMember.removeFailed")}
          </Text>
        ) : null}
      </Body>
      <Ctx
        cr={group?.name ?? upper(t("mobile:group.common.fallbackName"))}
        name={t("group.members")}
      />
    </Screen>
  );
}

export default observer(Members);
