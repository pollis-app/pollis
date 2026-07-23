import { useMemo, useState } from "react";
import { View, Text } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
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
} from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

function Members() {
  const router = useRouter();
  const { groupId } = useLocalSearchParams<{ groupId?: string }>();
  const id = groupId ?? null;
  const currentUser = appStore.currentUser;

  const { data: members = [], isLoading } = useGroupMembers(id);
  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === id);
  const removeMember = useRemoveMember(id);
  const setRole = useSetMemberRole(id);

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
          { label: "GROUPS" },
          { label: group?.name ?? "Group" },
          { label: "Members", leaf: true },
        ]}
        end={String(members.length || 0)}
      />
      <Body>
        <SectionTitle>MEMBERS</SectionTitle>
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
            Loading…
          </Text>
        ) : null}
        {members.map((m) => {
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
                  {isMe ? " · you" : ""}
                </Text>
              }
              sub={
                isOwner
                  ? "Owner"
                  : isAdmin
                    ? "Admin"
                    : `joined ${new Date(m.joined_at).toLocaleDateString()}`
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
                      accessibilityLabel={isAdmin ? "Remove admin" : "Make admin"}
                      onPress={() => onToggleRole(m.user_id, m.role)}
                    >
                      {setRole.isPending ? "…" : isAdmin ? "Admin" : "Make admin"}
                    </Chip>
                    <Chip
                      variant={armed ? "on" : "default"}
                      testID={`btn-remove-member-${m.user_id}`}
                      accessibilityLabel="Remove member"
                      onPress={() => onRemove(m.user_id)}
                    >
                      {removeMember.isPending && armed
                        ? "…"
                        : armed
                          ? "Confirm"
                          : "Remove"}
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
            {(removeMember.error as Error).message || "Couldn't remove member."}
          </Text>
        ) : null}
      </Body>
      <Ctx cr={group?.name ?? "GROUP"} name="Members" />
    </Screen>
  );
}

export default observer(Members);
