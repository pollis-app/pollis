import { useCallback, useEffect, useRef, useState } from "react";
import { BackHandler, View, Text, type TextInput } from "react-native";
import { useFocusEffect, useLocalSearchParams, useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import {
  Screen,
  Header,
  Body,
  Group,
  ListRow,
  Field,
  IconButton,
  Button,
} from "../../components/ui";
import { GlobalSearchResults } from "../../components/search/GlobalSearchResults";
import { Icon } from "../../components/icons";
import { ProfileCard } from "../../components/self/ProfileCard";
import { accentDisplayName } from "../../components/self/accentName";
import { useNotificationPermission } from "../../components/self/useNotificationPermission";
import { confirmSignOut } from "../../components/self/confirmSignOut";
import { VersionRow, UpdateRow } from "../../components/self/BuildRows";
import { useTheme } from "../../components/theme";
import { semantic, type as ty, fonts, space } from "../../theme/tokens";
import { languageOption } from "../../i18n/languages";
import { useUserProfile, useLogout, useUserDevices } from "../../hooks/queries";
import { autoLockLabel, useAutoLockMinutes, useLockNow } from "../../lib/autolock";
import { appStore } from "../../stores/appStore";
import { onTabReselect } from "../../lib/tabReselect";

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

  // Global search (#1550), Settings-style: focusing or typing swaps the menu
  // for results in place, with no navigation.
  const params = useLocalSearchParams<{ search?: string }>();
  const searchRef = useRef<TextInput>(null);
  const [q, setQ] = useState("");
  // The exact-match account lookup, run on return only (#1216).
  const [userQuery, setUserQuery] = useState("");
  const [searchFocused, setSearchFocused] = useState(false);
  const searching = searchFocused || q.length > 0;
  const trimmed = q.trim();

  const endSearch = () => {
    setQ("");
    setUserQuery("");
    setSearchFocused(false);
    searchRef.current?.blur();
  };

  useEffect(() => onTabReselect("self", endSearch), []);

  // Android back ends a search before it leaves Self. Only while Self is the
  // focused screen: a page pushed on top keeps its own back.
  useFocusEffect(
    useCallback(() => {
      if (!searching) {
        return undefined;
      }
      const sub = BackHandler.addEventListener("hardwareBackPress", () => {
        endSearch();
        return true;
      });
      return () => sub.remove();
    }, [searching]),
  );

  // A group's "Search <group>" lands here with `search=1`: open the field.
  useEffect(() => {
    if (params.search) {
      searchRef.current?.focus();
      router.setParams({ search: undefined });
    }
  }, [params.search, router]);

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
      // `useLogout` ends the session either way, which lands on sign-in.
      logout.mutate();
    });
  };

  const appearanceName = t("mobile:self.hub.appearance");

  return (
    <Screen testID="screen-self" aboveTabBar>
      <Header variant="large" title={t("mobile:self.title")} />
      <View
        style={{
          flexDirection: "row",
          alignItems: "center",
          gap: space.xs,
          paddingHorizontal: space.xxl,
          paddingBottom: space.sm,
        }}
      >
        <Field
          ref={searchRef}
          testID="input-search"
          accessibilityLabel={t("search:page.title")}
          containerStyle={{ flex: 1 }}
          value={q}
          onChangeText={setQ}
          onFocus={() => setSearchFocused(true)}
          onBlur={() => setSearchFocused(false)}
          onSubmitEditing={() => {
            if (trimmed.length >= 2) {
              setUserQuery(trimmed);
            }
          }}
          returnKeyType="search"
          autoCorrect={false}
          placeholder={t("search.placeholder")}
          icon={<Icon.search size={18} color={semantic.muted} />}
          trailing={
            q.length > 0 ? (
              <IconButton
                testID="btn-search-clear"
                accessibilityLabel={t("search.clear")}
                tone="neutral"
                icon={<Icon.x size={18} />}
                onPress={endSearch}
                style={{ marginEnd: -space.md }}
              />
            ) : null
          }
        />
        {searching ? (
          <Button testID="btn-search-cancel" variant="subtle" onPress={endSearch}>
            {t("common:actions.cancel")}
          </Button>
        ) : null}
      </View>
      {searching ? (
        <GlobalSearchResults q={q} setQ={setQ} userQuery={userQuery} />
      ) : (
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
            <ListRow
              testID="row-self-diagnostics"
              glyph={<Icon.diagnostics size={GLYPH} color={semantic.text} />}
              name={t("settings:diagnostics.title")}
              chevron
              onPress={() => router.push("/self/diagnostics")}
            />
            <VersionRow />
            <UpdateRow />
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
      )}
    </Screen>
  );
}

export default observer(Self);
