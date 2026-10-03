import { useEffect, useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  Avatar,
  Field,
  Ctx,
  Button,
  BottomAction,
} from "../../components/ui";
import { FormField, FormStack } from "../../components/FormField";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { upper } from "../../i18n";
import { useUserProfile, useUpdateProfile } from "../../hooks/queries";
import { isValidUsername } from "../../lib/username";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

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

  const avatarLabel = (handle || currentUser?.username || "us").slice(0, 2);

  return (
    <Screen testID="screen-self-user-settings" centered>
      <Crumb
        segs={[
          { label: upper(t("mobile:self.title")) },
          { label: t("user.title"), leaf: true },
        ]}
      />
      <Body>
        <View
          style={{
            flexDirection: "row",
            alignItems: "center",
            gap: 14,
            paddingHorizontal: 18,
            paddingTop: 14,
            paddingBottom: 8,
          }}
        >
          <Avatar label={avatarLabel} size="lg" variant="amber" />
          <View style={{ flex: 1 }}>
            <Text
              style={{
                fontFamily: ty.h1.fontFamily,
                fontSize: 18,
                color: semantic.ink,
              }}
            >
              {displayName || handle || "—"}
            </Text>
            {isLoading ? (
              <Text
                style={{
                  fontFamily: ty.body.fontFamily,
                  fontSize: 12,
                  color: semantic.mute,
                }}
              >
                {t("common:states.loading")}
              </Text>
            ) : null}
          </View>
        </View>

        <SectionTitle>{upper(t("mobile:self.identityHeading"))}</SectionTitle>
        <FormStack>
          <FormField label={t("mobile:self.userSettings.displayName")}>
            <Field
              value={displayName}
              onChangeText={setDisplayName}
              testID="input-display-name"
              accessibilityLabel={t("mobile:self.userSettings.displayName")}
            />
          </FormField>
          <FormField
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
              icon={
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    color: semantic.mute,
                  }}
                >
                  @
                </Text>
              }
            />
          </FormField>
          <FormField label={t("user.emailLabel")}>
            <Field
              value={profile?.email ?? currentUser?.email ?? ""}
              editable={false}
              testID="input-email"
              accessibilityLabel={t("user.emailLabel")}
              icon={<Icon.mail color={semantic.mute} />}
            />
            <Button
              variant="subtle"
              full
              testID="btn-change-email"
              onPress={() => router.push("/self/change-email")}
              icon={<Icon.edit color={semantic.ink} />}
            >
              {t("user.changeEmailButton")}
            </Button>
          </FormField>
        </FormStack>

        {updateProfile.isError ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.danger,
              paddingHorizontal: 18,
              paddingTop: 10,
            }}
          >
            {(updateProfile.error as Error).message || t("user.saveFailed")}
          </Text>
        ) : null}
        {updateProfile.isSuccess && !dirty ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.accent,
              paddingHorizontal: 18,
              paddingTop: 10,
            }}
          >
            {t("user.saved")}
          </Text>
        ) : null}
      </Body>
      <Ctx cr={upper(t("mobile:self.title"))} name={t("user.title")} />
      <BottomAction>
        <Button
          full
          testID="btn-save"
          variant="primary"
          onPress={onSave}
          disabled={!dirty || !nextHandle || handleInvalid || updateProfile.isPending}
          iconRight={<Icon.check color="#0a0907" />}
        >
          {updateProfile.isPending
            ? upper(t("user.saving"))
            : upper(t("user.saveButton"))}
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
