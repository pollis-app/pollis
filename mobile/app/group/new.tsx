import { useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Crumb,
  Body,
  Field,
  Button,
  BottomAction,
} from "../../components/ui";
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
        createDefaultTextChannel: true,
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
        <View style={{ paddingHorizontal: 18, paddingTop: 12, gap: 16 }}>
          <View style={{ gap: 8 }}>
            <Text style={ty.label}>{upper(t("createGroup.nameLabel"))}</Text>
            <Field
              testID="input-group-name"
              accessibilityLabel={t("createGroup.nameLabel")}
              amber
              value={name}
              onChangeText={setName}
              placeholder={t("createGroup.namePlaceholder")}
              icon={<Icon.people color={semantic.mute} />}
            />
          </View>
          <View style={{ gap: 8 }}>
            <Text style={ty.label}>
              {upper(t("mobile:group.new.descriptionLabel"))}
            </Text>
            <Field
              testID="input-group-description"
              accessibilityLabel={t("mobile:group.common.descriptionLabel")}
              value={description}
              onChangeText={setDescription}
              placeholder={t("mobile:group.new.descriptionPlaceholder")}
            />
          </View>
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
        </View>
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
