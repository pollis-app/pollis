import { useEffect, useState } from "react";
import { View, Text } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Field,
  Button,
  BottomAction,
  Chip,
  Ctx,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import {
  useGroupChannels,
  useUserGroupsWithChannels,
  useUpdateGroup,
  useDeleteGroup,
  useDeleteChannel,
  useGroupMembers,
} from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

function GroupSettings() {
  const router = useRouter();
  const { groupId } = useLocalSearchParams<{ groupId?: string }>();
  const id = groupId ?? null;
  const currentUser = appStore.currentUser;

  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === id);
  const { data: channels = [] } = useGroupChannels(id);
  const { data: members = [] } = useGroupMembers(id);
  const updateGroup = useUpdateGroup(id);
  const deleteGroup = useDeleteGroup();
  const deleteChannel = useDeleteChannel(id);

  const myRole = members.find((m) => m.user_id === currentUser?.id)?.role;
  const iAmAdmin = myRole === "admin" || myRole === "owner";
  const iAmOwner = myRole === "owner";

  const [name, setName] = useState(group?.name ?? "");
  const [description, setDescription] = useState(group?.description ?? "");
  const [seeded, setSeeded] = useState(false);
  const [confirmDeleteGroup, setConfirmDeleteGroup] = useState(false);
  const [confirmDeleteChannel, setConfirmDeleteChannel] = useState<string | null>(null);

  useEffect(() => {
    if (group && !seeded) {
      setName(group.name);
      setDescription(group.description ?? "");
      setSeeded(true);
    }
  }, [group, seeded]);

  const dirty =
    group != null &&
    (name !== group.name || description !== (group.description ?? ""));

  const onSave = () => {
    if (!name.trim()) {
      return;
    }
    updateGroup.mutate({
      name: name.trim(),
      description: description.trim() || undefined,
    });
  };

  const onDeleteGroup = () => {
    if (!confirmDeleteGroup) {
      setConfirmDeleteGroup(true);
      return;
    }
    if (!id) {
      return;
    }
    deleteGroup.mutate(id, {
      onSuccess: () => router.replace("/(tabs)/groups"),
    });
  };

  const onDeleteChannel = (channelId: string) => {
    if (confirmDeleteChannel !== channelId) {
      setConfirmDeleteChannel(channelId);
      return;
    }
    deleteChannel.mutate(channelId, {
      onSettled: () => setConfirmDeleteChannel(null),
    });
  };

  return (
    <Screen>
      <Crumb
        segs={[
          { label: "GROUPS" },
          { label: group?.name ?? "Group" },
          { label: "Settings", leaf: true },
        ]}
      />
      <Body>
        {!iAmAdmin ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingTop: 14,
            }}
          >
            You don't have admin permissions for this group.
          </Text>
        ) : null}

        <SectionTitle>IDENTITY</SectionTitle>
        <View style={{ paddingHorizontal: 18, paddingTop: 6, gap: 6 }}>
          <Text style={ty.label}>NAME</Text>
          <Field value={name} onChangeText={setName} editable={iAmAdmin} />
        </View>
        <View style={{ paddingHorizontal: 18, paddingTop: 14, gap: 6 }}>
          <Text style={ty.label}>DESCRIPTION</Text>
          <Field
            value={description}
            onChangeText={setDescription}
            editable={iAmAdmin}
          />
        </View>

        <SectionTitle>CHANNELS</SectionTitle>
        {channels.map((c) => {
          const armed = confirmDeleteChannel === c.id;
          return (
            <ListRow
              key={c.id}
              minHeight={48}
              glyph={<Icon.hash color={semantic.mute} />}
              name={c.name}
              nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
              sub={c.description ?? undefined}
              end={
                iAmAdmin && channels.length > 1 ? (
                  <Chip
                    variant={armed ? "on" : "default"}
                    onPress={() => onDeleteChannel(c.id)}
                  >
                    {deleteChannel.isPending && armed
                      ? "…"
                      : armed
                        ? "Confirm"
                        : "Delete"}
                  </Chip>
                ) : null
              }
            />
          );
        })}
        {channels.length === 0 ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingTop: 6,
            }}
          >
            No channels.
          </Text>
        ) : null}

        {iAmOwner ? (
          <View>
            <SectionTitle>DANGER</SectionTitle>
            <View style={{ paddingHorizontal: 18 }}>
              <Button
                full
                variant="danger"
                icon={<Icon.exit color={semantic.danger} />}
                onPress={onDeleteGroup}
                disabled={deleteGroup.isPending}
              >
                {deleteGroup.isPending
                  ? "DELETING…"
                  : confirmDeleteGroup
                    ? "TAP AGAIN TO CONFIRM"
                    : "DELETE GROUP"}
              </Button>
              {deleteGroup.isError ? (
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 12,
                    color: semantic.danger,
                    paddingTop: 6,
                  }}
                >
                  {(deleteGroup.error as Error).message ||
                    "Couldn't delete group."}
                </Text>
              ) : null}
            </View>
          </View>
        ) : null}

        {updateGroup.isError ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.danger,
              paddingHorizontal: 18,
              paddingTop: 6,
            }}
          >
            {(updateGroup.error as Error).message || "Couldn't save changes."}
          </Text>
        ) : null}
      </Body>
      <Ctx cr={group?.name ?? "GROUP"} name="Settings" />
      {iAmAdmin ? (
        <BottomAction>
          <Button
            full
            variant="primary"
            onPress={onSave}
            disabled={!dirty || !name.trim() || updateGroup.isPending}
            iconRight={<Icon.check color="#0a0907" />}
          >
            {updateGroup.isPending ? "SAVING…" : "SAVE CHANGES"}
          </Button>
        </BottomAction>
      ) : null}
    </Screen>
  );
}

export default observer(GroupSettings);
