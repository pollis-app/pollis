import { useEffect, useState } from "react";
import { View, Text } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import { useTranslation } from "react-i18next";
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
import { upper } from "../../i18n";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

function GroupSettings() {
  const { t } = useTranslation("channels");
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
    <Screen testID="screen-group-settings">
      <Crumb
        segs={[
          { label: upper(t("nav:breadcrumb.groups")) },
          { label: group?.name ?? t("mobile:group.common.fallbackName") },
          { label: t("nav:breadcrumb.settings"), leaf: true },
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
            {t("mobile:group.settings.notAdmin")}
          </Text>
        ) : null}

        <SectionTitle>{upper(t("mobile:group.settings.identitySection"))}</SectionTitle>
        <View style={{ paddingHorizontal: 18, paddingTop: 6, gap: 6 }}>
          <Text style={ty.label}>{upper(t("renameGroup.nameLabel"))}</Text>
          <Field
            value={name}
            onChangeText={setName}
            editable={iAmAdmin}
            testID="input-group-name"
            accessibilityLabel={t("renameGroup.nameLabel")}
          />
        </View>
        <View style={{ paddingHorizontal: 18, paddingTop: 14, gap: 6 }}>
          <Text style={ty.label}>{upper(t("renameGroup.descriptionLabel"))}</Text>
          <Field
            value={description}
            onChangeText={setDescription}
            editable={iAmAdmin}
            testID="input-group-description"
            accessibilityLabel={t("mobile:group.common.descriptionLabel")}
          />
        </View>

        <SectionTitle>{upper(t("mobile:group.settings.channelsSection"))}</SectionTitle>
        {channels.map((c) => {
          const armed = confirmDeleteChannel === c.id;
          return (
            <ListRow
              key={c.id}
              testID={`row-channel-${c.id}`}
              minHeight={48}
              glyph={<Icon.hash color={semantic.mute} />}
              name={c.name}
              nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
              sub={c.description ?? undefined}
              end={
                iAmAdmin && channels.length > 1 ? (
                  <Chip
                    variant={armed ? "on" : "default"}
                    testID={`btn-delete-channel-${c.id}`}
                    accessibilityLabel={t("channel.deleteLabel")}
                    onPress={() => onDeleteChannel(c.id)}
                  >
                    {deleteChannel.isPending && armed
                      ? "…"
                      : armed
                        ? t("mobile:group.common.confirm")
                        : t("common:actions.delete")}
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
            {t("mobile:group.common.noChannels")}
          </Text>
        ) : null}

        <SectionTitle>{upper(t("mobile:group.common.emoji"))}</SectionTitle>
        <ListRow
          testID="row-group-emoji"
          minHeight={48}
          glyph={<Icon.plus color={semantic.mute} />}
          name={t("group.customEmoji")}
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          sub={t("mobile:group.settings.customEmojiSub")}
          onPress={() =>
            id
              ? router.push({
                  pathname: "/group/emoji",
                  params: { groupId: id },
                })
              : undefined
          }
        />

        {iAmOwner ? (
          <View>
            <SectionTitle>{upper(t("mobile:group.common.danger"))}</SectionTitle>
            <View style={{ paddingHorizontal: 18 }}>
              <Button
                full
                testID="btn-delete-group"
                variant="danger"
                icon={<Icon.exit color={semantic.danger} />}
                onPress={onDeleteGroup}
                disabled={deleteGroup.isPending}
              >
                {deleteGroup.isPending
                  ? upper(t("mobile:group.settings.deleting"))
                  : confirmDeleteGroup
                    ? upper(t("mobile:group.settings.tapAgainToConfirm"))
                    : upper(t("mobile:group.settings.deleteGroup"))}
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
                    t("mobile:group.settings.deleteFailed")}
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
            {(updateGroup.error as Error).message || t("renameGroup.renameFailed")}
          </Text>
        ) : null}
      </Body>
      <Ctx
        cr={group?.name ?? upper(t("mobile:group.common.fallbackName"))}
        name={t("nav:breadcrumb.settings")}
      />
      {iAmAdmin ? (
        <BottomAction>
          <Button
            full
            testID="btn-save"
            variant="primary"
            onPress={onSave}
            disabled={!dirty || !name.trim() || updateGroup.isPending}
            iconRight={<Icon.check color="#0a0907" />}
          >
            {updateGroup.isPending
              ? upper(t("renameGroup.submitting"))
              : upper(t("renameGroup.submit"))}
          </Button>
        </BottomAction>
      ) : null}
    </Screen>
  );
}

export default observer(GroupSettings);
