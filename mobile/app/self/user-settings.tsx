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
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { upper } from "../../i18n";
import { useUserProfile, useUpdateProfile } from "../../hooks/queries";
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

  const onSave = () => {
    if (!handle.trim()) {
      return;
    }
    updateProfile.mutate({
      username: handle.trim(),
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
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.mute,
              }}
            >
              {isLoading
                ? t("common:states.loading")
                : t("mobile:self.userSettings.avatarHint")}
            </Text>
          </View>
        </View>

        <SectionTitle>{upper(t("mobile:self.identityHeading"))}</SectionTitle>
        <View style={{ paddingHorizontal: 18, paddingTop: 6, gap: 6 }}>
          <Text style={ty.label}>
            {upper(t("mobile:self.userSettings.displayName"))}
          </Text>
          <Field
            value={displayName}
            onChangeText={setDisplayName}
            testID="input-display-name"
            accessibilityLabel={t("mobile:self.userSettings.displayName")}
          />
        </View>
        <View style={{ paddingHorizontal: 18, paddingTop: 14, gap: 6 }}>
          <Text style={ty.label}>
            {upper(t("mobile:self.userSettings.handle"))}
          </Text>
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
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              color: semantic.mute,
            }}
          >
            {t("mobile:self.userSettings.handleHint")}
          </Text>
        </View>
        <View style={{ paddingHorizontal: 18, paddingTop: 14, gap: 6 }}>
          <Text style={ty.label}>{upper(t("user.emailLabel"))}</Text>
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
        </View>

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
          disabled={!dirty || !handle.trim() || updateProfile.isPending}
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
