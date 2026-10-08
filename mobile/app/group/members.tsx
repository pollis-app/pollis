import { useMemo, useState } from "react";
import { View } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Header, Body, ListRow, Group, Avatar, Chip } from "../../components/ui";
import { Hint, ErrorText } from "../../components/groups/FormBits";
import { space } from "../../theme/tokens";
import {
  useGroupMembers,
  useRemoveMember,
  useSetMemberRole,
  useUserGroupsWithChannels,
  sortMembersByRole,
} from "../../hooks/queries";
import { activeLocale } from "../../i18n";
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
      <Header
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
              const handle = `@${m.username ?? m.user_id.slice(0, 8)}`;
              const name = isMe ? `${handle} ${t("members.self")}` : handle;
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
                    glyph={<Avatar label={m.username || m.user_id} variant={isMe ? "self" : "default"} />}
                    name={name}
                    sub={role}
                    chevron={!isMe}
                    onPress={
                      isMe
                        ? undefined
                        : () =>
                            router.push({
                              pathname: "/user/[id]",
                              params: { id: m.user_id },
                            })
                    }
                  />
                  {/* Admin actions sit under the member, not inside the
                      row, so the row stays one control with one label. */}
                  {iAmAdmin && !isMe && !isOwner ? (
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
                      <Chip
                        variant="outline"
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
