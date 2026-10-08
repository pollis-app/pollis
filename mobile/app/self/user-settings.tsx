import { useEffect, useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import {
  Screen,
  Header,
  Body,
  SectionTitle,
  Avatar,
  Field,
  Button,
  BottomAction,
} from "../../components/ui";
import { SettingsField, ErrorText, ReadOnlyField } from "../../components/self/SettingsField";
import { Icon } from "../../components/icons";
import { semantic, type as ty, space } from "../../theme/tokens";
import { useUserProfile, useUpdateProfile } from "../../hooks/queries";
import { isValidUsername } from "../../lib/username";
import { appStore } from "../../stores/appStore";

function UserSettings() {
  const { t } = useTranslation("settings");
  const router = useRouter();
  const currentUser = appStore.currentUser;
  const { data: profile, isLoading } = useUserProfile();
  const updateProfile = useUpdateProfile();

  const [displayName, setDisplayName] = useState("");
  const [handle, setHandle] = useState("");

  // Seed local form state once the profile loads, then leave it alone so
  // the user's in-progress edits aren't clobbered by a background refetch.
  const [seeded, setSeeded] = useState(false);
  useEffect(() => {
    if (!seeded && profile) {
      setDisplayName(profile.preferred_name ?? "");
      setHandle(profile.username ?? "");
      setSeeded(true);
    }
  }, [profile, seeded]);

  const dirty =
    profile != null &&
    (displayName !== (profile.preferred_name ?? "") ||
      handle !== (profile.username ?? ""));

  // The DS refuses a username outside its rule (`is_valid_username`) and
  // passes an UNCHANGED one through, because default names that predate the
  // rule must still be able to save a display name. Same carve-out here, so
  // the screen explains the refusal before the round trip rather than after.
  const nextHandle = handle.trim();
  const handleInvalid =
    nextHandle !== (profile?.username ?? "") && !isValidUsername(nextHandle);

  const onSave = () => {
    if (!nextHandle || handleInvalid) {
      return;
    }
    updateProfile.mutate({
      username: nextHandle,
      preferredName: displayName.trim() || undefined,
    });
  };

  const shownName = displayName || handle || currentUser?.username || "";

  return (
    <Screen testID="screen-self-user-settings">
      <Header title={t("mobile:self.hub.accountDetails")} backTo={t("mobile:self.title")} />
      <Body contentContainerStyle={{ paddingHorizontal: space.xxl, paddingTop: space.xxl, gap: space.xxxl }}>
        <View style={{ flexDirection: "row", alignItems: "center", gap: space.xl }}>
          <Avatar label={shownName} size="lg" variant="self" />
          <View style={{ flex: 1, minWidth: 0 }}>
            <Text accessibilityRole="header" numberOfLines={1} ellipsizeMode="tail" style={ty.title}>
              {shownName || "—"}
            </Text>
            {isLoading ? <Text style={ty.meta}>{t("common:states.loading")}</Text> : null}
          </View>
        </View>

        <View style={{ gap: space.xxl }}>
          <SectionTitle style={{ paddingHorizontal: 0, paddingTop: 0, paddingBottom: 0 }}>
            {t("mobile:self.identityHeading")}
          </SectionTitle>
          <SettingsField label={t("mobile:self.userSettings.displayName")}>
            <Field
              value={displayName}
              onChangeText={setDisplayName}
              testID="input-display-name"
              accessibilityLabel={t("mobile:self.userSettings.displayName")}
            />
          </SettingsField>
          <SettingsField
            label={t("mobile:self.userSettings.handle")}
            hint={t("mobile:self.userSettings.handleHint")}
            error={handleInvalid && nextHandle ? t("mobile:self.userSettings.handleInvalid") : null}
            errorTestID="text-handle-invalid"
          >
            <Field
              value={handle}
              onChangeText={setHandle}
              testID="input-handle"
              accessibilityLabel={t("mobile:self.userSettings.handle")}
              icon={<Text style={[ty.body, { color: semantic.muted }]}>@</Text>}
            />
          </SettingsField>
        </View>

        <View style={{ gap: space.lg }}>
          <SettingsField label={t("user.emailLabel")}>
            <ReadOnlyField
              value={profile?.email ?? currentUser?.email ?? ""}
              testID="input-email"
              accessibilityLabel={t("user.emailLabel")}
              icon={<Icon.mail size={18} color={semantic.muted} />}
            />
          </SettingsField>
          <Button
            testID="btn-change-email"
            onPress={() => router.push("/self/change-email")}
            icon={<Icon.pencil size={18} color={semantic.text} />}
          >
            {t("mobile:self.changeEmail.title")}
          </Button>
        </View>

        {updateProfile.isError ? (
          <ErrorText>
            {(updateProfile.error as Error).message || t("user.saveFailed")}
          </ErrorText>
        ) : null}
        {updateProfile.isSuccess && !dirty ? (
          <View style={{ flexDirection: "row", alignItems: "center", gap: space.xs }}>
            <Icon.check size={16} color={semantic.accent} />
            <Text accessibilityRole="alert" style={[ty.secondary, { color: semantic.text }]}>
              {t("user.saved")}
            </Text>
          </View>
        ) : null}
      </Body>
      <BottomAction>
        <Button
          full
          testID="btn-save"
          variant="primary"
          onPress={onSave}
          disabled={!dirty || !nextHandle || handleInvalid || updateProfile.isPending}
          iconRight={<Icon.check size={18} color={semantic.onAccent} />}
        >
          {updateProfile.isPending
            ? t("user.saving")
            : t("mobile:self.userSettings.save")}
        </Button>
        <Button
          variant="subtle"
          full
          testID="btn-cancel"
          onPress={() => router.back()}
        >
          {t("common:actions.cancel")}
        </Button>
      </BottomAction>
    </Screen>
  );
}

export default observer(UserSettings);
