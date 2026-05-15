import { useState } from "react";
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

const SWATCHES = [
  { n: "Amber", c: DEFAULT_ACCENT_HEX },
  { n: "Citron", c: "#c9d65a" },
  { n: "Mint", c: "#8ad6a7" },
  { n: "Glass", c: "#7ec5d6" },
  { n: "Lilac", c: "#bda3e0" },
  { n: "Rust", c: "#d68f5a" },
];

const BEHAVIOR = [
  { n: "Show inline timestamps", on: true },
  { n: "Show member avatars", on: true },
  { n: "Mark verified peers with ◆", on: true },
  { n: "Read receipts", on: false },
  { n: "Reduce motion", on: false },
];

export default function Preferences() {
  const { accentHex, setAccent } = useTheme();
  const [theme, setTheme] = useState("Coal");
  const [density, setDensity] = useState("Compact");
  const [toggles, setToggles] = useState(BEHAVIOR.map((b) => b.on));

  return (
    <Screen>
      <Crumb segs={[{ label: "SELF" }, { label: "Preferences", leaf: true }]} />
      <Body>
        <View style={{ paddingHorizontal: 18, paddingTop: 12 }}>
          <Text style={[ty.label, { marginBottom: 10 }]}>ACCENT</Text>
          <View
            style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}
          >
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
            {["Coal", "Paper", "System"].map((opt) => (
              <Chip
                key={opt}
                variant={theme === opt ? "on" : "default"}
                onPress={() => setTheme(opt)}
              >
                {opt}
              </Chip>
            ))}
          </View>
        </View>

        <View style={{ paddingHorizontal: 18, paddingTop: 18 }}>
          <Text style={[ty.label, { marginBottom: 10 }]}>DENSITY</Text>
          <View style={{ flexDirection: "row", gap: 8 }}>
            {["Compact", "Comfortable"].map((opt) => (
              <Chip
                key={opt}
                variant={density === opt ? "on" : "default"}
                onPress={() => setDensity(opt)}
              >
                {opt}
              </Chip>
            ))}
          </View>
        </View>

        <SectionTitle>BEHAVIOR</SectionTitle>
        {BEHAVIOR.map((b, i) => (
          <ListRow
            key={b.n}
            minHeight={46}
            name={b.n}
            nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
            end={
              <Toggle
                on={toggles[i]}
                onPress={() =>
                  setToggles((t) =>
                    t.map((v, idx) => (idx === i ? !v : v))
                  )
                }
              />
            }
          />
        ))}
      </Body>
      <Ctx cr="SELF" name="Preferences" />
    </Screen>
  );
}
