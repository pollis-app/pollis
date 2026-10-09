import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { Screen, Header, Body, Group, ListRow } from "../../components/ui";
import { Icon } from "../../components/icons";
import { ProfileCard } from "../../components/self/ProfileCard";
import { accentDisplayName } from "../../components/self/accentName";
import { useNotificationPermission } from "../../components/self/useNotificationPermission";
import { confirmSignOut } from "../../components/self/confirmSignOut";
import { useTheme } from "../../components/theme";
import { semantic, type as ty, fonts, space } from "../../theme/tokens";
import { languageOption } from "../../i18n/languages";
import { useUserProfile, useLogout, useUserDevices } from "../../hooks/queries";
import { autoLockLabel, useAutoLockMinutes, useLockNow } from "../../lib/autolock";
import { appStore } from "../../stores/appStore";

const GLYPH = 22;

function Self() {
  const { t, i18n } = useTranslation("mobile");
  const router = useRouter();
  const { accentHex } = useTheme();
  const currentUser = appStore.currentUser;
  const { data: profile } = useUserProfile();
  const { data: devices } = useUserDevices();
  const logout = useLogout();
  const { info: notif } = useNotificationPermission();
  const { minutes: autoLockMinutes } = useAutoLockMinutes();
  const lockNow = useLockNow();

  const handle =
    profile?.username ?? currentUser?.username ?? t("mobile:user.fallbackHandle");
  const display = profile?.preferred_name || handle;

  const accentName = accentDisplayName(t, accentHex);
  const notifValue = notif
    ? notif.granted
      ? t("mobile:self.preferences.notificationsOn")
      : t("mobile:self.hub.off")
    : undefined;
  const languageLabel = languageOption(i18n.language)?.label;
  const autoLockValue = autoLockLabel(autoLockMinutes);
  const deviceValue = devices
    ? t("mobile:self.hub.deviceCount", { count: devices.length })
    : undefined;

  const onSignOut = () => {
    if (logout.isPending) {
      return;
    }
    confirmSignOut(() => {
      logout.mutate(undefined, {
        onSuccess: () => router.replace("/(auth)/email"),
        onError: () => router.replace("/(auth)/email"),
      });
    });
  };

  const appearanceName = t("mobile:self.hub.appearance");

  return (
    <Screen testID="screen-self" aboveTabBar>
      <Header variant="large" title={t("mobile:self.title")} />
      <Body
        contentContainerStyle={{
          paddingHorizontal: space.xxl,
          paddingTop: 4,
          gap: space.xxxl,
        }}
      >
        <ProfileCard
          displayName={display}
          handle={handle}
          onEdit={() => router.push("/self/user-settings")}
        />

        <Group title={t("mobile:self.hub.accountSection")}>
          <ListRow
            testID="row-self-user-settings"
            glyph={<Icon.user size={GLYPH} color={semantic.text} />}
            name={t("mobile:self.hub.accountDetails")}
            chevron
            onPress={() => router.push("/self/user-settings")}
          />
          <ListRow
            testID="row-self-security"
            glyph={<Icon.shield size={GLYPH} color={semantic.text} />}
            name={t("mobile:self.hub.securityDevices")}
            value={deviceValue}
            chevron
            onPress={() => router.push("/self/security")}
          />
          <ListRow
            testID="row-self-saved"
            glyph={<Icon.bookmark size={GLYPH} color={semantic.text} />}
            name={t("mobile:self.hub.savedMessages")}
            chevron
            onPress={() => router.push("/self/saved")}
          />
        </Group>

        <Group title={t("mobile:self.hub.appSection")}>
          <ListRow
            testID="row-self-preferences"
            glyph={<Icon.appearance size={GLYPH} color={semantic.text} />}
            name={appearanceName}
            accessibilityLabel={`${appearanceName}, ${accentName}`}
            end={
              <View style={{ flexDirection: "row", alignItems: "center", gap: space.xs }}>
                <View
                  style={{
                    width: 14,
                    height: 14,
                    borderRadius: 7,
                    backgroundColor: semantic.accent,
                  }}
                />
                <Text numberOfLines={1} style={[ty.secondary, { color: semantic.muted }]}>
                  {accentName}
                </Text>
              </View>
            }
            chevron
            onPress={() => router.push("/self/preferences")}
          />
          <ListRow
            testID="row-self-notifications"
            glyph={<Icon.bell size={GLYPH} color={semantic.text} />}
            name={t("settings:notifications.heading")}
            value={notifValue}
            chevron
            onPress={() =>
              router.push({
                pathname: "/self/preferences",
                params: { section: "notifications" },
              })
            }
          />
          <ListRow
            testID="row-self-language"
            glyph={<Icon.globe size={GLYPH} color={semantic.text} />}
            name={t("settings:language.heading")}
            value={languageLabel}
            chevron
            onPress={() =>
              router.push({
                pathname: "/self/preferences",
                params: { section: "language" },
              })
            }
          />
          <ListRow
            testID="row-self-autolock"
            glyph={<Icon.lock size={GLYPH} color={semantic.text} />}
            name={t("settings:security.autoLockHeading")}
            value={autoLockValue}
            chevron
            onPress={() =>
              router.push({
                pathname: "/self/security",
                params: { section: "autolock" },
              })
            }
          />
        </Group>

        <Group>
          <ListRow
            testID="btn-lock-now"
            glyph={<Icon.lockKeyhole size={GLYPH} color={semantic.text} />}
            name={t("mobile:self.security.lockNow")}
            accessibilityHint={t("mobile:self.security.lockNowSub")}
            onPress={() => void lockNow()}
          />
          <ListRow
            testID="btn-sign-out"
            glyph={<Icon.logOut size={GLYPH} color={semantic.text} />}
            name={
              logout.isPending
                ? t("mobile:self.hub.signingOut")
                : t("mobile:self.hub.signOut")
            }
            nameStyle={{ fontFamily: fonts.semibold }}
            disabled={logout.isPending}
            onPress={onSignOut}
          />
        </Group>
      </Body>
    </Screen>
  );
}

export default observer(Self);
