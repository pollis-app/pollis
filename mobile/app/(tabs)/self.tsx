import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Crumb,
  Body,
  ListRow,
  Avatar,
  Diamond,
  Button,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty, r } from "../../theme/tokens";
import { upper } from "../../i18n";
import { useUserProfile, useLogout } from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

function Self() {
  const { t } = useTranslation("mobile");
  const router = useRouter();
  const currentUser = appStore.currentUser;
  const { data: profile } = useUserProfile();
  const logout = useLogout();

  const handle =
    profile?.username ?? currentUser?.username ?? t("mobile:user.fallbackHandle");
  const display = profile?.preferred_name || handle;
  const avatarLabel = (handle || "us").slice(0, 2);

  const onSignOut = () => {
    logout.mutate(undefined, {
      onSuccess: () => router.replace("/(auth)/email"),
      onError: () => router.replace("/(auth)/email"),
    });
  };

  const cards = [
    {
      g: <Icon.gear color={semantic.accent} />,
      n: t("settings:preferences.title"),
      s: t("mobile:self.hub.preferencesSub"),
      to: "/self/preferences" as const,
      t: "row-self-preferences",
    },
    {
      g: <Icon.user color={semantic.accent} />,
      n: t("settings:user.title"),
      s: t("mobile:self.hub.userSettingsSub"),
      to: "/self/user-settings" as const,
      t: "row-self-user-settings",
    },
    {
      g: <Icon.shield color={semantic.accent} />,
      n: t("settings:security.title"),
      s: t("mobile:self.hub.securitySub"),
      to: "/self/security" as const,
      t: "row-self-security",
    },
    {
      g: <Icon.bookmark color={semantic.accent} />,
      n: t("saved:page.title"),
      s: t("mobile:self.hub.savedSub"),
      to: "/self/saved" as const,
      t: "row-self-saved",
    },
  ];

  return (
    <Screen testID="screen-self">
      <Crumb
        segs={[{ label: upper(t("mobile:self.title")), leaf: true }]}
        end={upper(t("common:presence.online"))}
      />
      <Body>
        <View
          style={{
            flexDirection: "row",
            alignItems: "center",
            gap: 14,
            paddingHorizontal: 18,
            paddingTop: 12,
            paddingBottom: 18,
          }}
        >
          <Avatar label={avatarLabel} size="lg" variant="amber" />
          <View style={{ flex: 1 }}>
            <Text
              style={{
                fontFamily: ty.h1.fontFamily,
                fontSize: 20,
                color: semantic.ink,
              }}
            >
              {display}
            </Text>
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.mute,
              }}
            >
              @{handle}
            </Text>
          </View>
          <View
            style={{ flexDirection: "row", alignItems: "center", gap: 6 }}
          >
            <Diamond size={6} />
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 11,
                letterSpacing: 1.1,
                color: semantic.accent,
              }}
            >
              {upper(t("common:presence.online"))}
            </Text>
          </View>
        </View>

        <View style={{ paddingHorizontal: 14, gap: 8 }}>
          {cards.map((c) => (
            <View
              key={c.t}
              style={{
                borderWidth: 1,
                borderColor: semantic.hair,
                borderRadius: r.lg,
                backgroundColor: semantic.fieldBg,
              }}
            >
              <ListRow
                testID={c.t}
                minHeight={64}
                glyph={c.g}
                name={c.n}
                sub={c.s}
                onPress={() => router.push(c.to)}
                end={<Icon.fwd color={semantic.mute} />}
              />
            </View>
          ))}
        </View>

        <View style={{ paddingHorizontal: 18, paddingTop: 10 }}>
          <Button
            testID="btn-sign-out"
            full
            variant="danger"
            icon={<Icon.exit color={semantic.danger} />}
            onPress={onSignOut}
            disabled={logout.isPending}
          >
            {logout.isPending
              ? upper(t("mobile:self.hub.signingOut"))
              : upper(t("auth:shell.signOutTitle"))}
          </Button>
        </View>
      </Body>
    </Screen>
  );
}

export default observer(Self);
