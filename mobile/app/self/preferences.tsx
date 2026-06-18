import { useCallback, useState } from "react";
import { View, Text, Pressable } from "react-native";
import { useFocusEffect } from "expo-router";
import { useObserver } from "mobx-react-lite";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Chip,
  Toggle,
  Ctx,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { useTheme } from "../../components/theme";
import { semantic, type as ty, r, DEFAULT_ACCENT_HEX } from "../../theme/tokens";
import { usePreferences } from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import {
  getPushPermissionInfo,
  ensurePushRegistration,
  openNotificationSettings,
} from "../../lib/push";

const SWATCHES = [
  { n: "Amber", c: DEFAULT_ACCENT_HEX },
  { n: "Citron", c: "#c9d65a" },
  { n: "Mint", c: "#8ad6a7" },
  { n: "Glass", c: "#7ec5d6" },
  { n: "Lilac", c: "#bda3e0" },
  { n: "Rust", c: "#d68f5a" },
];

const BEHAVIOR_KEYS = [
  { key: "show_inline_timestamps", n: "Show inline timestamps", defaultOn: true },
  { key: "show_member_avatars", n: "Show member avatars", defaultOn: true },
  { key: "mark_verified_peers", n: "Mark verified peers with ◆", defaultOn: true },
  { key: "read_receipts", n: "Read receipts", defaultOn: false },
  { key: "reduce_motion", n: "Reduce motion", defaultOn: false },
] as const;

const THEMES = ["Coal", "Paper", "System"] as const;
const DENSITIES = ["Compact", "Comfortable"] as const;

// Notification permission status + control. The OS permission is the source
// of truth (we can't toggle it from JS), so this reflects it and routes the
// tap correctly: fire the in-app OS prompt while it's still undetermined, else
// deep-link to system Settings (where a prior allow/deny can be changed).
function NotificationsSetting() {
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const [info, setInfo] = useState<{
    granted: boolean;
    canAskAgain: boolean;
  } | null>(null);

  const refresh = useCallback(() => {
    void getPushPermissionInfo()
      .then(setInfo)
      .catch(() => {});
  }, []);

  // Re-check on focus so returning from system Settings reflects the change.
  useFocusEffect(refresh);

  const granted = info?.granted ?? false;
  const sub = granted
    ? "On — new messages will notify you"
    : info && !info.canAskAgain
      ? "Off — enable in system Settings"
      : "Off — tap to enable";

  const onPress = () => {
    void (async () => {
      if (!granted && info?.canAskAgain && userId) {
        // Still undetermined — fire the single in-app OS prompt.
        await ensurePushRegistration(userId);
      } else {
        // Granted (manage / turn off there) or denied (only Settings can
        // re-enable — the in-app prompt is spent).
        await openNotificationSettings();
      }
      refresh();
    })();
  };

  return (
    <ListRow
      minHeight={46}
      name="Notifications"
      nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
      sub={sub}
      onPress={onPress}
      end={<Toggle on={granted} onPress={onPress} />}
    />
  );
}

export default function Preferences() {
  const { accentHex, setAccent } = useTheme();
  const { data: prefs, update } = usePreferences();

  const theme = prefs?.mobile_theme ?? "Coal";
  const density = prefs?.mobile_density ?? "Compact";
  const behavior = prefs?.mobile_behavior ?? {};

  const isBehaviorOn = (key: string, fallback: boolean): boolean => {
    const v = behavior[key];
    return typeof v === "boolean" ? v : fallback;
  };

  const toggleBehavior = (key: string, fallback: boolean) => {
    const next = { ...behavior, [key]: !isBehaviorOn(key, fallback) };
    update({ mobile_behavior: next });
  };

  return (
    <Screen>
      <Crumb segs={[{ label: "SELF" }, { label: "Preferences", leaf: true }]} />
      <Body>
        <View style={{ paddingHorizontal: 18, paddingTop: 12 }}>
          <Text style={[ty.label, { marginBottom: 10 }]}>ACCENT</Text>
          <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
            {SWATCHES.map((s) => {
              const sel = accentHex.toLowerCase() === s.c.toLowerCase();
              return (
                <Pressable
                  key={s.n}
                  onPress={() => setAccent(s.c)}
                  style={{
                    width: "31.5%",
                    borderWidth: 1,
                    borderColor: sel ? s.c : semantic.hair,
                    backgroundColor: sel
                      ? semantic.accentSoft
                      : "transparent",
                    paddingVertical: 10,
                    paddingHorizontal: 10,
                    borderRadius: r.sm,
                    flexDirection: "row",
                    alignItems: "center",
                    gap: 8,
                  }}
                >
                  <View
                    style={{
                      width: 14,
                      height: 14,
                      backgroundColor: s.c,
                      borderRadius: r.sm,
                    }}
                  />
                  <Text
                    style={{
                      fontFamily: ty.body.fontFamily,
                      fontSize: 13,
                      color: semantic.ink,
                    }}
                  >
                    {s.n}
                  </Text>
                  {sel && (
                    <View style={{ marginLeft: "auto" }}>
                      <Icon.check color={s.c} />
                    </View>
                  )}
                </Pressable>
              );
            })}
          </View>
        </View>

        <View style={{ paddingHorizontal: 18, paddingTop: 18 }}>
          <Text style={[ty.label, { marginBottom: 10 }]}>THEME</Text>
          <View style={{ flexDirection: "row", gap: 8 }}>
            {THEMES.map((opt) => (
              <Chip
                key={opt}
                variant={theme === opt ? "on" : "default"}
                onPress={() => update({ mobile_theme: opt })}
              >
                {opt}
              </Chip>
            ))}
          </View>
        </View>

        <View style={{ paddingHorizontal: 18, paddingTop: 18 }}>
          <Text style={[ty.label, { marginBottom: 10 }]}>DENSITY</Text>
          <View style={{ flexDirection: "row", gap: 8 }}>
            {DENSITIES.map((opt) => (
              <Chip
                key={opt}
                variant={density === opt ? "on" : "default"}
                onPress={() => update({ mobile_density: opt })}
              >
                {opt}
              </Chip>
            ))}
          </View>
        </View>

        <SectionTitle>BEHAVIOR</SectionTitle>
        {BEHAVIOR_KEYS.map((b) => (
          <ListRow
            key={b.key}
            minHeight={46}
            name={b.n}
            nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
            end={
              <Toggle
                on={isBehaviorOn(b.key, b.defaultOn)}
                onPress={() => toggleBehavior(b.key, b.defaultOn)}
              />
            }
          />
        ))}

        <SectionTitle>NOTIFICATIONS</SectionTitle>
        <NotificationsSetting />
      </Body>
      <Ctx cr="SELF" name="Preferences" />
    </Screen>
  );
}
