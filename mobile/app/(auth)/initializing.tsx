import { useEffect } from "react";
import { View, Text, Pressable } from "react-native";
import Svg, { Rect } from "react-native-svg";
import { useRouter } from "expo-router";
import { Screen, Crumb, Card } from "../../components/ui";
import { PollisMark } from "../../components/PollisMark";
import { palette, semantic, type as ty } from "../../theme/tokens";

const STEPS = [
  { n: "KEYS LOADED", s: "OK", done: true },
  { n: "DEVICE PAIRED", s: "OK", done: true },
  { n: "SYNC GROUPS", s: "62%", done: false },
  { n: "RESOLVE PEERS", s: "—", done: false, muted: true },
];

function Corner({ pos }: { pos: "tl" | "tr" | "bl" | "br" }) {
  const top = pos[0] === "t";
  const left = pos[1] === "l";
  return (
    <View
      style={{
        position: "absolute",
        width: 14,
        height: 14,
        opacity: 0.75,
        [top ? "top" : "bottom"]: 10,
        [left ? "left" : "right"]: 10,
        borderColor: semantic.ink,
        borderTopWidth: top ? 1 : 0,
        borderBottomWidth: top ? 0 : 1,
        borderLeftWidth: left ? 1 : 0,
        borderRightWidth: left ? 0 : 1,
      }}
    />
  );
}

export default function Initializing() {
  const router = useRouter();

  useEffect(() => {
    const t = setTimeout(() => router.replace("/(tabs)/groups"), 2600);
    return () => clearTimeout(t);
  }, [router]);

  return (
    <Screen>
      <Corner pos="tl" />
      <Corner pos="tr" />
      <Corner pos="bl" />
      <Corner pos="br" />
      <Crumb segs={[{ label: "INITIALIZING", leaf: true }]} end="62%" />

      <View
        style={{
          position: "absolute",
          top: 0,
          left: 0,
          right: 0,
          bottom: 0,
          opacity: 0.4,
        }}
        pointerEvents="none"
      >
        <Svg width="100%" height="100%" viewBox="0 0 390 844">
          {Array.from({ length: 180 }).map((_, i) => {
            const x = (i * 37) % 390;
            const y = (i * 53) % 844;
            const w = (i * 7) % 5 < 2 ? 3 : 2;
            return (
              <Rect
                key={i}
                x={x}
                y={y}
                width={w}
                height={w}
                fill="rgb(230,182,90)"
                opacity={((i % 9) + 3) / 24}
              />
            );
          })}
        </Svg>
      </View>

      <View
        style={{
          flex: 1,
          alignItems: "center",
          justifyContent: "center",
          paddingHorizontal: 24,
        }}
      >
        <Card
          style={{
            width: "100%",
            borderColor: semantic.accent,
            backgroundColor: "rgba(10,9,7,.85)",
            padding: 22,
          }}
        >
          <View style={{ marginBottom: 18 }}>
            <PollisMark />
          </View>
          <Text
            style={{
              fontFamily: ty.h1.fontFamily,
              fontSize: 22,
              color: semantic.ink,
              marginBottom: 6,
            }}
          >
            Setting up
          </Text>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              marginBottom: 20,
            }}
          >
            One moment — pairing your device and syncing keys.
          </Text>
          <View
            style={{
              height: 2,
              backgroundColor: semantic.hair,
              marginBottom: 20,
            }}
          >
            <View
              style={{
                height: 2,
                width: "62%",
                backgroundColor: semantic.accent,
              }}
            />
          </View>
          {STEPS.map((r2, i) => (
            <View
              key={i}
              style={{
                flexDirection: "row",
                justifyContent: "space-between",
                paddingVertical: 4,
                opacity: r2.muted ? 0.5 : 1,
              }}
            >
              <Text style={[ty.label, { fontSize: 10 }]}>
                <Text
                  style={{
                    color: r2.done ? semantic.accent : semantic.mute,
                  }}
                >
                  {r2.done ? "◆" : "◇"}
                </Text>{" "}
                {r2.n}
              </Text>
              <Text
                style={[
                  ty.label,
                  { fontSize: 10, color: r2.done ? semantic.accent : semantic.ink2 },
                ]}
              >
                {r2.s}
              </Text>
            </View>
          ))}
        </Card>
      </View>

      <View
        style={{
          flexDirection: "row",
          justifyContent: "space-between",
          alignItems: "center",
          paddingHorizontal: 24,
          paddingVertical: 14,
        }}
      >
        <Text style={ty.label}>v3.1.2 · NODE 0x4A2C</Text>
        <Pressable onPress={() => router.replace("/(tabs)/groups")}>
          <Text style={[ty.label, { color: semantic.accent }]}>SKIP →</Text>
        </Pressable>
      </View>
    </Screen>
  );
}
