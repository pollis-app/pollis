import { useState } from "react";
import { Pressable, View, Text } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Crumb,
  Body,
  Field,
  Button,
  BottomAction,
  Toggle,
} from "../../components/ui";
import { FormField, FormStack } from "../../components/FormField";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { useCreateGroup } from "../../hooks/queries";
import { upper } from "../../i18n";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

function NewGroup() {
  const { t } = useTranslation("channels");
  const router = useRouter();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [createTextChannel, setCreateTextChannel] = useState(false);
  const createGroup = useCreateGroup();
  const setSelectedGroupId = appStore.setSelectedGroupId;

  const onSubmit = () => {
    const trimmedName = name.trim();
    if (!trimmedName) {
      return;
    }
    createGroup.mutate(
      {
        name: trimmedName,
        description: description.trim() || undefined,
        createDefaultTextChannel: createTextChannel,
      },
      {
        onSuccess: (group) => {
          setSelectedGroupId(group.id);
          // The default text channel created server-side is fetched
          // lazily by the groups list; land the user on the group page
          // so they see the new channel render in once the query
          // refetches.
          router.replace({
            pathname: "/group/[id]",
            params: { id: group.id },
          });
        },
      },
    );
  };

  return (
    <Screen testID="screen-group-new" centered>
      <Crumb
        segs={[
          { label: upper(t("nav:breadcrumb.groups")) },
          { label: t("mobile:group.new.crumb"), leaf: true },
        ]}
      />
      <Body>
        <FormStack paddingTop={12}>
          <FormField label={t("createGroup.nameLabel")}>
            <Field
              testID="input-group-name"
              accessibilityLabel={t("createGroup.nameLabel")}
              amber
              value={name}
              onChangeText={setName}
              placeholder={t("createGroup.namePlaceholder")}
              icon={<Icon.people color={semantic.mute} />}
            />
          </FormField>
          <FormField label={t("mobile:group.new.descriptionLabel")}>
            <Field
              testID="input-group-description"
              accessibilityLabel={t("mobile:group.common.descriptionLabel")}
              value={description}
              onChangeText={setDescription}
              placeholder={t("mobile:group.new.descriptionPlaceholder")}
            />
          </FormField>
          {/* Opt-in, off by default, like desktop. No voice option: voice is
              not supported on mobile. */}
          <Pressable
            accessibilityRole="switch"
            accessibilityState={{ checked: createTextChannel }}
            onPress={() => setCreateTextChannel((v) => !v)}
            style={{ flexDirection: "row", alignItems: "center", gap: 14 }}
          >
            <View style={{ flex: 1, gap: 4 }}>
              <Text style={{ fontFamily: ty.body.fontFamily, fontSize: 14, color: semantic.ink }}>
                {t("createGroup.textChannelLabel")}
              </Text>
              <Text style={{ fontFamily: ty.body.fontFamily, fontSize: 11, lineHeight: 16, color: semantic.mute }}>
                {t("createGroup.textChannelDescription")}
              </Text>
            </View>
            <Toggle
              testID="toggle-general-channel"
              on={createTextChannel}
              onPress={() => setCreateTextChannel((v) => !v)}
              accessibilityLabel={t("createGroup.textChannelLabel")}
            />
          </Pressable>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              color: semantic.mute,
              lineHeight: 16,
            }}
          >
            {t("mobile:group.new.blurb")}
          </Text>
          {createGroup.isError ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.danger,
              }}
            >
              {(createGroup.error as Error).message ||
                t("createGroup.createFailed")}
            </Text>
          ) : null}
        </FormStack>
      </Body>
      <BottomAction>
        <Button
          full
          variant="primary"
          onPress={onSubmit}
          disabled={!name.trim() || createGroup.isPending}
          iconRight={<Icon.arrowRight color="#0a0907" />}
        >
          {createGroup.isPending
            ? upper(t("createGroup.submitting"))
            : upper(t("createGroup.submit"))}
        </Button>
        <Button variant="subtle" full onPress={() => router.back()}>
          {t("common:actions.cancel")}
        </Button>
      </BottomAction>
    </Screen>
  );
}

export default observer(NewGroup);
