import { ScrollView, Text, View, useWindowDimensions } from "react-native";
import { useLocalSearchParams } from "expo-router";
import { useObserver } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { Screen, Header, Group } from "../../components/ui";
import { LanguageSection } from "../../components/LanguageSection";
import { AccentSwatch } from "../../components/self/AccentSwatch";
import { ChoiceGrid } from "../../components/self/ChoiceGrid";
import { ChoiceChip } from "../../components/self/ChoiceChip";
import { ToggleRow } from "../../components/self/ToggleRow";
import { accentPresetLabel } from "../../components/self/accentName";
import { useNotificationPermission } from "../../components/self/useNotificationPermission";
import { useSectionScroll } from "../../components/self/useSectionScroll";
import { useTheme } from "../../components/theme";
import { semantic, type as ty, fonts, space, ACCENT_PRESETS } from "../../theme/tokens";
import { usePreferences, useNotificationContent } from "../../hooks/queries";
import {
  NOTIFICATION_CONTENT_OPTIONS,
  type NotificationContent,
} from "../../lib/notificationContent";
import { appStore } from "../../stores/appStore";
import { ensurePushRegistration, openNotificationSettings } from "../../lib/push";
import type { TFunction } from "i18next";

const SWATCHES = ACCENT_PRESETS;

// Read receipts are NOT here: they are the synced top-level `send_read_receipts` key.
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

// The OS permission is the source of truth: prompt while undetermined, else open Settings.
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
        await ensurePushRegistration(userId);
      } else {
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

function notificationContentLabel(t: TFunction, value: NotificationContent): string {
  switch (value) {
    case "full":
      return t("mobile:self.preferences.notificationContent.full");
    case "name":
      return t("mobile:self.preferences.notificationContent.name");
    case "none":
      return t("mobile:self.preferences.notificationContent.none");
  }
}

// #1599: what a push shows. The extension decrypts on the device and reads this from the core DB.
function NotificationContentSetting() {
  const { t } = useTranslation("settings");
  const { value, set } = useNotificationContent();
  // An even grid that loses columns as Dynamic Type grows, like auto-lock.
  const { fontScale } = useWindowDimensions();
  const columns = fontScale >= 1.6 ? 1 : fontScale >= 1.25 ? 2 : 3;
  const heading = t("mobile:self.preferences.notificationContent.heading");

  return (
    <View testID="pref-notification-content" style={{ padding: space.xxl, gap: space.md }}>
      <View style={{ gap: 2 }}>
        <Text style={{ fontFamily: fonts.medium, fontSize: 16, color: semantic.text }}>
          {heading}
        </Text>
        <Text style={ty.secondary}>
          {t("mobile:self.preferences.notificationContent.description")}
        </Text>
      </View>
      <ChoiceGrid columns={columns} accessibilityRole="radiogroup" accessibilityLabel={heading}>
        {NOTIFICATION_CONTENT_OPTIONS.map((opt) => (
          <ChoiceChip
            key={opt}
            testID={`chip-notification-content-${opt}`}
            label={notificationContentLabel(t, opt)}
            selected={value === opt}
            onPress={() => {
              if (value !== opt) {
                set.mutate(opt);
              }
            }}
          />
        ))}
      </ChoiceGrid>
    </View>
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
    <Screen testID="screen-self-preferences">
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
            {/* A fixed 3 × 2 grid, so swatches line up on both platforms. */}
            <ChoiceGrid
              columns={3}
              gap={space.xs}
              accessibilityRole="radiogroup"
              accessibilityLabel={t("mobile:self.preferences.accentHeading")}
              style={{ padding: space.sm }}
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
            </ChoiceGrid>
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
            <NotificationContentSetting />
          </Group>
        </View>
      </ScrollView>
    </Screen>
  );
}
