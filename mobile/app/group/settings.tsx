import { useEffect, useState } from "react";
import { View } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Header,
  Body,
  SectionTitle,
  ListRow,
  Group,
  Field,
  Button,
  BottomAction,
  Chip,
} from "../../components/ui";
import { LabeledField, Hint, ErrorText } from "../../components/groups/FormBits";
import { Icon } from "../../components/icons";
import { semantic, space } from "../../theme/tokens";
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
      <Header
        title={t("mobile:group.settings.title")}
        subtitle={group?.name}
      />
      <Body contentContainerStyle={{ paddingHorizontal: space.xxl }}>
        {!iAmAdmin ? (
          <View style={{ paddingTop: space.xxl }}>
            <Hint>{t("mobile:group.settings.notAdmin")}</Hint>
          </View>
        ) : null}

        <SectionTitle style={{ paddingHorizontal: 4 }}>
          {t("mobile:group.settings.identitySection")}
        </SectionTitle>
        <View style={{ gap: space.xxl }}>
          <LabeledField label={t("mobile:group.settings.nameLabel")}>
            <Field
              value={name}
              onChangeText={setName}
              editable={iAmAdmin}
              autoCapitalize="words"
              testID="input-group-name"
              accessibilityLabel={t("mobile:group.settings.nameLabel")}
            />
          </LabeledField>
          <LabeledField label={t("renameGroup.descriptionLabel")}>
            <Field
              value={description}
              onChangeText={setDescription}
              editable={iAmAdmin}
              autoCapitalize="sentences"
              testID="input-group-description"
              accessibilityLabel={t("mobile:group.common.descriptionLabel")}
            />
          </LabeledField>
          {updateGroup.isError ? (
            <ErrorText>
              {(updateGroup.error as Error).message || t("renameGroup.renameFailed")}
            </ErrorText>
          ) : null}
        </View>

        <SectionTitle style={{ paddingHorizontal: 4 }}>
          {t("mobile:group.settings.channelsSection")}
        </SectionTitle>
        {channels.length === 0 ? (
          <Hint>{t("mobile:group.common.noChannels")}</Hint>
        ) : (
          <Group>
            {channels.map((c) => {
              const armed = confirmDeleteChannel === c.id;
              return (
                <ListRow
                  key={c.id}
                  testID={`row-channel-${c.id}`}
                  glyph={<Icon.hash size={16} color={semantic.dim} />}
                  name={c.name}
                  sub={c.description || undefined}
                  end={
                    iAmAdmin && channels.length > 1 ? (
                      <Chip
                        variant="outline"
                        selected={armed}
                        testID={`btn-delete-channel-${c.id}`}
                        accessibilityLabel={
                          armed
                            ? t("mobile:group.settings.tapAgainToConfirm")
                            : t("mobile:group.settings.deleteChannel", { name: c.name })
                        }
                        leading={<Icon.trash size={14} color={armed ? semantic.accent : semantic.text} />}
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
          </Group>
        )}

        <SectionTitle style={{ paddingHorizontal: 4 }}>{t("mobile:group.common.emoji")}</SectionTitle>
        <Group>
          <ListRow
            testID="row-group-emoji"
            glyph={<Icon.smile size={20} color={semantic.dim} />}
            name={t("mobile:group.panel.customEmoji")}
            sub={t("mobile:group.settings.customEmojiSub")}
            chevron
            onPress={() =>
              id
                ? router.push({
                    pathname: "/group/emoji",
                    params: { groupId: id },
                  })
                : undefined
            }
          />
        </Group>

        {iAmOwner ? (
          <View style={{ paddingTop: space.xxxl * 2, gap: space.sm }}>
            {/* Destructive, on its own at the end, two taps to confirm. */}
            <Group>
              <ListRow
                testID="btn-delete-group"
                glyph={<Icon.trash size={20} color={semantic.accent} />}
                name={
                  deleteGroup.isPending
                    ? t("mobile:group.settings.deleting")
                    : confirmDeleteGroup
                      ? t("mobile:group.settings.tapAgainToConfirm")
                      : t("mobile:group.settings.deleteGroup")
                }
                nameStyle={{ color: semantic.accent }}
                disabled={deleteGroup.isPending}
                onPress={onDeleteGroup}
              />
            </Group>
            {deleteGroup.isError ? (
              <ErrorText>
                {(deleteGroup.error as Error).message ||
                  t("mobile:group.settings.deleteFailed")}
              </ErrorText>
            ) : null}
          </View>
        ) : null}
      </Body>
      {iAmAdmin ? (
        <BottomAction>
          <Button
            full
            testID="btn-save"
            variant="primary"
            onPress={onSave}
            disabled={!dirty || !name.trim() || updateGroup.isPending}
          >
            {updateGroup.isPending ? t("renameGroup.submitting") : t("renameGroup.submit")}
          </Button>
        </BottomAction>
      ) : null}
    </Screen>
  );
}

export default observer(GroupSettings);
