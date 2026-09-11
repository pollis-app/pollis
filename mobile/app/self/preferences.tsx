import { useCallback, useState } from "react";
import { View, Text, Pressable } from "react-native";
import { useFocusEffect } from "expo-router";
import { useObserver } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
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
import { LanguageSection } from "../../components/LanguageSection";
import { useTheme } from "../../components/theme";
import { semantic, type as ty, r, DEFAULT_ACCENT_HEX } from "../../theme/tokens";
import { upper } from "../../i18n";
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
] as const;

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

const THEMES = ["Coal", "Paper", "System"] as const;
const DENSITIES = ["Compact", "Comfortable"] as const;

// The wire values above stay English; only the rendered label is keyed, one
// literal call per value so `i18n-check` can see every key.
function swatchLabel(t: TFunction, n: (typeof SWATCHES)[number]["n"]): string {
  switch (n) {
    case "Amber":
      return t("mobile:self.preferences.swatch.amber");
    case "Citron":
      return t("mobile:self.preferences.swatch.citron");
    case "Mint":
      return t("mobile:self.preferences.swatch.mint");
    case "Glass":
      return t("mobile:self.preferences.swatch.glass");
    case "Lilac":
      return t("mobile:self.preferences.swatch.lilac");
    case "Rust":
      return t("mobile:self.preferences.swatch.rust");
  }
}

function themeLabel(t: TFunction, opt: (typeof THEMES)[number]): string {
  switch (opt) {
    case "Coal":
      return t("mobile:self.preferences.theme.coal");
    case "Paper":
      return t("mobile:self.preferences.theme.paper");
    case "System":
      return t("mobile:self.preferences.theme.system");
  }
}

function densityLabel(t: TFunction, opt: (typeof DENSITIES)[number]): string {
  switch (opt) {
    case "Compact":
      return t("mobile:self.preferences.density.compact");
    case "Comfortable":
      return t("mobile:self.preferences.density.comfortable");
  }
}

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
    <ListRow
      minHeight={46}
      name={t("notifications.heading")}
      nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
      sub={sub}
      onPress={onPress}
      end={
        <Toggle
          on={granted}
          onPress={onPress}
          testID="toggle-notifications"
          accessibilityLabel={t("notifications.heading")}
        />
      }
    />
  );
}

export default function Preferences() {
  const { t } = useTranslation("settings");
  const { accentHex, setAccent } = useTheme();
  const { data: prefs, update } = usePreferences();

  const theme = prefs?.mobile_theme ?? "Coal";
  const density = prefs?.mobile_density ?? "Compact";
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
      <Crumb
        segs={[
          { label: upper(t("mobile:self.title")) },
          { label: t("preferences.title"), leaf: true },
        ]}
      />
      <Body>
        <View style={{ paddingHorizontal: 18, paddingTop: 12 }}>
          <Text style={[ty.label, { marginBottom: 10 }]}>
            {upper(t("mobile:self.preferences.accentHeading"))}
          </Text>
          <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
            {SWATCHES.map((s) => {
              const sel = accentHex.toLowerCase() === s.c.toLowerCase();
              const label = swatchLabel(t, s.n);
              return (
                <Pressable
                  key={s.n}
                  testID={`chip-accent-${s.n.toLowerCase()}`}
                  accessibilityLabel={t("mobile:self.preferences.accentA11y", {
                    name: label,
                  })}
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
                    {label}
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
          <Text style={[ty.label, { marginBottom: 10 }]}>
            {upper(t("mobile:self.preferences.themeHeading"))}
          </Text>
          <View style={{ flexDirection: "row", gap: 8 }}>
            {THEMES.map((opt) => (
              <Chip
                key={opt}
                testID={`chip-theme-${opt.toLowerCase()}`}
                variant={theme === opt ? "on" : "default"}
                onPress={() => update({ mobile_theme: opt })}
              >
                {themeLabel(t, opt)}
              </Chip>
            ))}
          </View>
        </View>

        <View style={{ paddingHorizontal: 18, paddingTop: 18 }}>
          <Text style={[ty.label, { marginBottom: 10 }]}>
            {upper(t("mobile:self.preferences.densityHeading"))}
          </Text>
          <View style={{ flexDirection: "row", gap: 8 }}>
            {DENSITIES.map((opt) => (
              <Chip
                key={opt}
                testID={`chip-density-${opt.toLowerCase()}`}
                variant={density === opt ? "on" : "default"}
                onPress={() => update({ mobile_density: opt })}
              >
                {densityLabel(t, opt)}
              </Chip>
            ))}
          </View>
        </View>

        <LanguageSection />

        <SectionTitle>{upper(t("voice:settings.behaviorHeading"))}</SectionTitle>
        {BEHAVIOR_KEYS.map((b) => (
          <ListRow
            key={b.key}
            minHeight={46}
            name={behaviorLabel(t, b.key)}
            nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
            end={
              <Toggle
                on={isBehaviorOn(b.key, b.defaultOn)}
                onPress={() => toggleBehavior(b.key, b.defaultOn)}
                testID={`toggle-${b.key.replace(/_/g, "-")}`}
                accessibilityLabel={behaviorLabel(t, b.key)}
              />
            }
          />
        ))}
        <ListRow
          minHeight={46}
          name={t("readReceipts.heading")}
          sub={t("mobile:self.preferences.readReceiptsSub")}
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          end={
            <Toggle
              on={sendReadReceipts}
              onPress={() => update({ send_read_receipts: !sendReadReceipts })}
              testID="toggle-read-receipts"
              accessibilityLabel={t("readReceipts.heading")}
            />
          }
        />

        <SectionTitle>{upper(t("notifications.heading"))}</SectionTitle>
        <NotificationsSetting />
      </Body>
      <Ctx cr={upper(t("mobile:self.title"))} name={t("preferences.title")} />
    </Screen>
  );
}
