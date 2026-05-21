import { View, Text } from "react-native";
import { useRouter } from "expo-router";
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
import { useUserProfile, useLogout } from "../../hooks/queries";
import { useAppStore } from "../../stores/appStore";

export default function Self() {
  const router = useRouter();
  const currentUser = useAppStore((s) => s.currentUser);
  const { data: profile } = useUserProfile();
  const logout = useLogout();

  const handle = profile?.username ?? currentUser?.username ?? "user";
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
      n: "Preferences",
      s: "Theme, density, behavior",
      to: "/self/preferences" as const,
    },
    {
      g: <Icon.user color={semantic.accent} />,
      n: "User settings",
      s: "Display name, handle, email",
      to: "/self/user-settings" as const,
    },
    {
      g: <Icon.shield color={semantic.accent} />,
      n: "Security",
      s: "Keys, devices, sign out",
      to: "/self/security" as const,
    },
  ];

  return (
    <Screen>
      <Crumb segs={[{ label: "SELF", leaf: true }]} end="ONLINE" />
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
              ONLINE
            </Text>
          </View>
        </View>

        <View style={{ paddingHorizontal: 14, gap: 8 }}>
          {cards.map((c) => (
            <View
              key={c.n}
              style={{
                borderWidth: 1,
                borderColor: semantic.hair,
                borderRadius: r.lg,
                backgroundColor: semantic.fieldBg,
              }}
            >
              <ListRow
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
            full
            variant="danger"
            icon={<Icon.exit color={semantic.danger} />}
            onPress={onSignOut}
            disabled={logout.isPending}
          >
            {logout.isPending ? "SIGNING OUT…" : "SIGN OUT"}
          </Button>
        </View>
      </Body>
    </Screen>
  );
}
