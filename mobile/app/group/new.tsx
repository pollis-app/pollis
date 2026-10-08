import { useState } from "react";
import { Pressable, View, Text } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Header,
  Body,
  Field,
  Button,
  BottomAction,
  Toggle,
  Card,
} from "../../components/ui";
import { LabeledField, Hint, ErrorText } from "../../components/groups/FormBits";
import { Icon } from "../../components/icons";
import { semantic, type as ty, space } from "../../theme/tokens";
import { useCreateGroup } from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { writeLastGroupId } from "../../components/groups/lastGroup";
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
          if (appStore.currentUser) {
            writeLastGroupId(appStore.currentUser.id, group.id);
          }
          // Land on the Groups tab with the new group selected (pill strip,
          // its channel panel, the tab bar) — not a standalone group page.
          // Its #General (if opted in) renders once the groups list
          // refetches. dismissTo pops this form back to the tabs.
          router.dismissTo("/(tabs)/groups");
        },
      },
    );
  };

  return (
    <Screen testID="screen-group-new">
      <Header title={t("mobile:group.new.title")} />
      <Body contentContainerStyle={{ padding: space.xxl, gap: space.xxl }}>
        <LabeledField label={t("mobile:group.new.nameLabel")}>
          <Field
            testID="input-group-name"
            accessibilityLabel={t("mobile:group.new.nameLabel")}
            autoCapitalize="words"
            value={name}
            onChangeText={setName}
            placeholder={t("createGroup.namePlaceholder")}
            icon={<Icon.users size={18} color={semantic.muted} />}
          />
        </LabeledField>
        <LabeledField label={t("mobile:group.new.descriptionLabel")}>
          <Field
            testID="input-group-description"
            accessibilityLabel={t("mobile:group.common.descriptionLabel")}
            autoCapitalize="sentences"
            value={description}
            onChangeText={setDescription}
            placeholder={t("mobile:group.new.descriptionPlaceholder")}
          />
        </LabeledField>
        {/* Opt-in, off by default, like desktop. No voice option: voice is
            not supported on mobile. */}
        <Card>
          <Pressable
            accessibilityRole="switch"
            accessibilityState={{ checked: createTextChannel }}
            accessibilityLabel={t("createGroup.textChannelLabel")}
            accessibilityHint={t("createGroup.textChannelDescription")}
            onPress={() => setCreateTextChannel((v) => !v)}
            style={{ flexDirection: "row", alignItems: "center", gap: space.xl }}
          >
            <View style={{ flex: 1, gap: 4 }}>
              <Text style={[ty.body, { color: semantic.text }]}>
                {t("createGroup.textChannelLabel")}
              </Text>
              <Text style={ty.secondary}>{t("createGroup.textChannelDescription")}</Text>
            </View>
            <Toggle
              testID="toggle-general-channel"
              on={createTextChannel}
              onPress={() => setCreateTextChannel((v) => !v)}
              accessibilityLabel={t("createGroup.textChannelLabel")}
            />
          </Pressable>
        </Card>
        <Hint>{t("mobile:group.new.blurb")}</Hint>
        {createGroup.isError ? (
          <ErrorText>
            {(createGroup.error as Error).message || t("createGroup.createFailed")}
          </ErrorText>
        ) : null}
      </Body>
      <BottomAction>
        <Button
          full
          testID="btn-submit-group"
          variant="primary"
          onPress={onSubmit}
          disabled={!name.trim() || createGroup.isPending}
        >
          {createGroup.isPending
            ? t("createGroup.submitting")
            : t("mobile:group.new.submit")}
        </Button>
      </BottomAction>
    </Screen>
  );
}

export default observer(NewGroup);
