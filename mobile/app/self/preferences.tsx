import { ScrollView, View } from "react-native";
import { useLocalSearchParams } from "expo-router";
import { useObserver } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { Screen, Header, Group } from "../../components/ui";
import { LanguageSection } from "../../components/LanguageSection";
import { AccentSwatch } from "../../components/self/AccentSwatch";
import { ToggleRow } from "../../components/self/ToggleRow";
import { accentPresetLabel } from "../../components/self/accentName";
import { useNotificationPermission } from "../../components/self/useNotificationPermission";
import { useSectionScroll } from "../../components/self/useSectionScroll";
import { useTheme } from "../../components/theme";
import { space, ACCENT_PRESETS } from "../../theme/tokens";
import { usePreferences } from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { ensurePushRegistration, openNotificationSettings } from "../../lib/push";
import type { TFunction } from "i18next";

// The presets live in theme/accents.ts so the contrast test can check each.
const SWATCHES = ACCENT_PRESETS;

// NOTE: read receipts are deliberately NOT in this list — they are not a
// mobile-local behavior toggle but the synced, desktop-shared top-level
// `send_read_receipts` key the Rust receipt chokepoints read (default true).
// The old `mobile_behavior.read_receipts` entry was a no-op: nested under a
// namespace the core never looks at, with an inverted default.
const BEHAVIOR_KEYS = [
  { key: "show_inline_timestamps", defaultOn: true },
  { key: "show_member_avatars", defaultOn: true },
  { key: "mark_verified_peers", defaultOn: true },
  { key: "reduce_motion", defaultOn: false },
] as const;

function behaviorLabel(
  t: TFunction,
  key: (typeof BEHAVIOR_KEYS)[number]["key"],
): string {
  switch (key) {
    case "show_inline_timestamps":
      return t("mobile:self.preferences.behavior.showInlineTimestamps");
    case "show_member_avatars":
      return t("mobile:self.preferences.behavior.showMemberAvatars");
    case "mark_verified_peers":
      return t("mobile:self.preferences.behavior.markVerifiedPeers");
    case "reduce_motion":
      return t("mobile:self.preferences.behavior.reduceMotion");
  }
}

// Notification permission status + control. The OS permission is the source
// of truth (we can't toggle it from JS), so this reflects it and routes the
// tap correctly: fire the in-app OS prompt while it's still undetermined, else
// deep-link to system Settings (where a prior allow/deny can be changed).
function NotificationsSetting() {
  const { t } = useTranslation("settings");
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const { info, refresh } = useNotificationPermission();

  const granted = info?.granted ?? false;
  const sub = granted
    ? t("mobile:self.preferences.notificationsOn")
    : info && !info.canAskAgain
      ? t("mobile:self.preferences.notificationsOffSettings")
      : t("mobile:self.preferences.notificationsOffTap");

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
    <ToggleRow
      testID="toggle-notifications"
      label={t("notifications.heading")}
      sub={sub}
      on={granted}
      onToggle={onPress}
    />
  );
}

export default function Preferences() {
  const { t } = useTranslation("settings");
  const { section } = useLocalSearchParams<{ section?: string }>();
  const { scrollRef, sectionLayout } = useSectionScroll(section);
  const { accentHex, setAccent } = useTheme();
  const { data: prefs, update } = usePreferences();

  const behavior = prefs?.mobile_behavior ?? {};
  const sendReadReceipts =
    typeof prefs?.send_read_receipts === "boolean"
      ? prefs.send_read_receipts
      : true;

  const isBehaviorOn = (key: string, fallback: boolean): boolean => {
    const v = behavior[key];
    return typeof v === "boolean" ? v : fallback;
  };

  const toggleBehavior = (key: string, fallback: boolean) => {
    const next = { ...behavior, [key]: !isBehaviorOn(key, fallback) };
    update({ mobile_behavior: next });
  };

  return (
    <Screen testID="screen-self-preferences" centered>
      <Header title={t("preferences.title")} backTo={t("mobile:self.title")} />
      <ScrollView
        ref={scrollRef}
        style={{ flex: 1 }}
        keyboardShouldPersistTaps="handled"
        contentContainerStyle={{
          paddingHorizontal: space.xxl,
          paddingTop: space.xxl,
          paddingBottom: space.xxxl,
          gap: space.xxxl,
        }}
      >
        <View onLayout={sectionLayout("accent")}>
          <Group title={t("mobile:self.preferences.accentHeading")}>
            <View
              accessibilityRole="radiogroup"
              accessibilityLabel={t("mobile:self.preferences.accentHeading")}
              style={{
                flexDirection: "row",
                flexWrap: "wrap",
                justifyContent: "space-around",
                rowGap: space.sm,
                padding: space.sm,
              }}
            >
              {SWATCHES.map((s) => {
                const label = accentPresetLabel(t, s.n);
                return (
                  <AccentSwatch
                    key={s.n}
                    testID={`chip-accent-${s.n.toLowerCase()}`}
                    color={s.c}
                    label={label}
                    selected={accentHex.toLowerCase() === s.c.toLowerCase()}
                    accessibilityLabel={t("mobile:self.preferences.accentA11y", {
                      name: label,
                    })}
                    onPress={() => setAccent(s.c)}
                  />
                );
              })}
            </View>
          </Group>
        </View>

        <LanguageSection onLayout={sectionLayout("language")} />

        <View onLayout={sectionLayout("behavior")}>
          <Group title={t("voice:settings.behaviorHeading")}>
            {BEHAVIOR_KEYS.map((b) => (
              <ToggleRow
                key={b.key}
                testID={`toggle-${b.key.replace(/_/g, "-")}`}
                label={behaviorLabel(t, b.key)}
                on={isBehaviorOn(b.key, b.defaultOn)}
                onToggle={() => toggleBehavior(b.key, b.defaultOn)}
              />
            ))}
            <ToggleRow
              testID="toggle-read-receipts"
              label={t("readReceipts.heading")}
              sub={t("mobile:self.preferences.readReceiptsSub")}
              on={sendReadReceipts}
              onToggle={() => update({ send_read_receipts: !sendReadReceipts })}
            />
          </Group>
        </View>

        <View onLayout={sectionLayout("notifications")}>
          <Group title={t("notifications.heading")}>
            <NotificationsSetting />
          </Group>
        </View>
      </ScrollView>
    </Screen>
  );
}
