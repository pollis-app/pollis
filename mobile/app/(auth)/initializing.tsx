import { useEffect, useRef, useState } from "react";
import { View, Text, Pressable } from "react-native";
import Svg, { Rect } from "react-native-svg";
import { useRouter } from "expo-router";
import { Screen, Crumb, Card } from "../../components/ui";
import { palette, semantic, type as ty } from "../../theme/tokens";
import { useInitializeIdentity } from "../../hooks/queries/useAuth";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

interface Step {
  n: string;
  s: string;
  done: boolean;
  muted?: boolean;
}

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

function Initializing() {
  const router = useRouter();
  const currentUser = appStore.currentUser;
  const initIdentity = useInitializeIdentity();
  const [error, setError] = useState<string | null>(null);

  const ranRef = useState({ ran: false })[0];
  const navTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Identity init often finishes in well under a frame. Navigating away the
  // instant it's done made the "Setting up" screen flash for a few frames —
  // too fast to read, so it registered as a glitch. Hold the screen for a
  // minimum dwell, then fade through to the app (the fade lives on the root
  // (tabs) screen).
  const MIN_VISIBLE_MS = 900;

  useEffect(() => {
    if (!currentUser || ranRef.ran) {
      return;
    }
    ranRef.ran = true;
    const startedAt = Date.now();
    initIdentity.mutate(currentUser.id, {
      onSuccess: () => {
        const wait = Math.max(0, MIN_VISIBLE_MS - (Date.now() - startedAt));
        navTimer.current = setTimeout(
          () => router.replace("/(tabs)/groups"),
          wait,
        );
      },
      onError: (e) => setError((e as Error).message || "Setup failed."),
    });
    return () => {
      if (navTimer.current) {
        clearTimeout(navTimer.current);
      }
    };
    // initIdentity is a stable mutation ref; intentionally fire once when
    // currentUser becomes available.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentUser?.id]);

  const progress = initIdentity.isPending
    ? "WORKING…"
    : initIdentity.isSuccess
      ? "DONE"
      : "READY";
  const steps: Step[] = [
    { n: "KEYS LOADED", s: "OK", done: true },
    { n: "DEVICE PAIRED", s: "OK", done: true },
    {
      n: "INITIALIZE IDENTITY",
      s: initIdentity.isPending
        ? "…"
        : initIdentity.isSuccess
          ? "OK"
          : initIdentity.isError
            ? "ERR"
            : "—",
      done: initIdentity.isSuccess,
    },
    { n: "RESOLVE PEERS", s: "—", done: false, muted: true },
  ];

  return (
    <Screen testID="screen-auth-initializing" centered>
      <Corner pos="tl" />
      <Corner pos="tr" />
      <Corner pos="bl" />
      <Corner pos="br" />
      <Crumb segs={[{ label: "INITIALIZING", leaf: true }]} end={progress} />

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
                width: initIdentity.isSuccess
                  ? "100%"
                  : initIdentity.isPending
                    ? "62%"
                    : "30%",
                backgroundColor: semantic.accent,
              }}
            />
          </View>
          {steps.map((r2, i) => (
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
                  style={{ color: r2.done ? semantic.accent : semantic.mute }}
                >
                  {r2.done ? "◆" : "◇"}
                </Text>{" "}
                {r2.n}
              </Text>
              <Text
                style={[
                  ty.label,
                  {
                    fontSize: 10,
                    color: r2.done ? semantic.accent : semantic.ink2,
                  },
                ]}
              >
                {r2.s}
              </Text>
            </View>
          ))}
          {error ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.danger,
                marginTop: 14,
              }}
            >
              {error}
            </Text>
          ) : null}
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
        <Pressable
          onPress={() => router.replace("/(tabs)/groups")}
          testID="btn-continue"
          accessibilityRole="button"
          accessibilityLabel="Skip"
        >
          <Text style={[ty.label, { color: semantic.accent }]}>SKIP →</Text>
        </Pressable>
      </View>
    </Screen>
  );
}

export default observer(Initializing);
