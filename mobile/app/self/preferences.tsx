import { View, Text, Pressable } from "react-native";
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
      </Body>
      <Ctx cr="SELF" name="Preferences" />
    </Screen>
  );
}
