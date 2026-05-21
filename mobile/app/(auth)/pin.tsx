import { useEffect, useState } from "react";
import { View, Text, Pressable } from "react-native";
import { useRouter } from "expo-router";
import { Screen, Crumb } from "../../components/ui";
import { PollisMark } from "../../components/PollisMark";
import { palette, semantic, type as ty, fonts, r } from "../../theme/tokens";
import {
  useSetPin,
  useUnlock,
  useUnlockState,
} from "../../hooks/queries/useAuth";
import { useAppStore } from "../../stores/appStore";

const SUBS = ["", "ABC", "DEF", "GHI", "JKL", "MNO", "PQRS", "TUV", "WXYZ"];

type Stage = "checking" | "create-first" | "create-confirm" | "unlock";

export default function AuthPIN() {
  const router = useRouter();
  const currentUser = useAppStore((s) => s.currentUser);
  const [pin, setPin] = useState("");
  const [firstPin, setFirstPin] = useState("");
  const [stage, setStage] = useState<Stage>("checking");
  const [error, setError] = useState<string | null>(null);

  const setPinMutation = useSetPin();
  const unlockMutation = useUnlock();
  const unlockState = useUnlockState();

  useEffect(() => {
    unlockState.mutate(undefined, {
      onSuccess: (snapshot) => {
        setStage(snapshot.pin_set ? "unlock" : "create-first");
      },
      onError: () => {
        // Treat a snapshot-read error as first-time setup; set_pin will
        // re-fail loudly if state is actually inconsistent.
        setStage("create-first");
      },
    });
    // unlockState is a stable mutation ref; intentionally fire only once
    // on mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const stageLabel = (() => {
    switch (stage) {
      case "checking":
        return "CHECKING…";
      case "create-first":
        return "STEP 1 OF 2 · ENTER PIN";
      case "create-confirm":
        return "STEP 2 OF 2 · CONFIRM PIN";
      case "unlock":
        return "ENTER PIN TO UNLOCK";
    }
  })();

  const headline = stage === "unlock" ? "Unlock Pollis" : "New device PIN";
  const subtitle =
    stage === "unlock"
      ? "Enter your device PIN to unlock encrypted local data."
      : "Used to unlock Pollis on this device. This stays on your phone — we never see it.";

  const onComplete = (entered: string) => {
    setError(null);
    if (stage === "unlock") {
      if (!currentUser) {
        setError("No active user. Sign in again.");
        setStage("checking");
        return;
      }
      unlockMutation.mutate(
        { userId: currentUser.id, pin: entered },
        {
          onSuccess: () => router.replace("/(auth)/initializing"),
          onError: (e) => {
            setError((e as Error).message || "Invalid PIN.");
            setPin("");
          },
        },
      );
      return;
    }
    if (stage === "create-first") {
      setFirstPin(entered);
      setPin("");
      setStage("create-confirm");
      return;
    }
    if (stage === "create-confirm") {
      if (entered !== firstPin) {
        setError("PINs didn't match. Start over.");
        setFirstPin("");
        setPin("");
        setStage("create-first");
        return;
      }
      setPinMutation.mutate(
        { newPin: entered },
        {
          onSuccess: () => {
            // First-device signup has a one-time recovery key stashed in
            // the store by `verify_otp`. Show it before initializing so
            // the user can save it before we drop it from memory.
            const pendingSecretKey =
              useAppStore.getState().pendingSecretKey;
            if (pendingSecretKey) {
              router.replace("/(auth)/emergency-kit");
            } else {
              router.replace("/(auth)/initializing");
            }
          },
          onError: (e) => {
            setError((e as Error).message || "Couldn't save PIN.");
            setFirstPin("");
            setPin("");
            setStage("create-first");
          },
        },
      );
    }
  };

  const push = (n: string) => {
    if (pin.length >= 4 || stage === "checking") {
      return;
    }
    const next = pin + n;
    setPin(next);
    if (next.length === 4) {
      setTimeout(() => onComplete(next), 120);
    }
  };

  const keys = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "", "0", "bk"];
  const busy =
    stage === "checking" ||
    setPinMutation.isPending ||
    unlockMutation.isPending;

  return (
    <Screen>
      <Crumb
        segs={[
          { label: "AUTH" },
          { label: stage === "unlock" ? "Unlock device" : "Set device PIN", leaf: true },
        ]}
      />
      <View style={{ flex: 1, paddingHorizontal: 24, paddingTop: 24, gap: 18 }}>
        <PollisMark />
        <View style={{ gap: 8 }}>
          <Text style={[ty.h1, { color: semantic.ink }]}>{headline}</Text>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              lineHeight: 19,
              color: semantic.mute,
            }}
          >
            {subtitle}
          </Text>
        </View>

        <View style={{ paddingVertical: 14 }}>
          <View
            style={{ flexDirection: "row", gap: 14, justifyContent: "center" }}
          >
            {[0, 1, 2, 3].map((i) => {
              const filled = i < pin.length;
              const cursor = i === pin.length;
              return (
                <View
                  key={i}
                  style={{
                    width: 52,
                    height: 60,
                    borderWidth: 1,
                    borderRadius: r.sm,
                    borderColor:
                      filled || cursor
                        ? semantic.accent
                        : semantic.hairStrong,
                    backgroundColor: semantic.fieldBg,
                    alignItems: "center",
                    justifyContent: "center",
                  }}
                >
                  {filled ? (
                    <Text
                      style={{
                        fontFamily: fonts.sora500,
                        fontSize: 24,
                        color: semantic.ink,
                      }}
                    >
                      •
                    </Text>
                  ) : cursor ? (
                    <View
                      style={{
                        width: 2,
                        height: 18,
                        backgroundColor: semantic.accent,
                      }}
                    />
                  ) : null}
                </View>
              );
            })}
          </View>
          <Text
            style={[ty.label, { textAlign: "center", marginTop: 14 }]}
          >
            {stageLabel}
          </Text>
          {error ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.danger,
                textAlign: "center",
                marginTop: 8,
              }}
            >
              {error}
            </Text>
          ) : null}
        </View>
      </View>

      <View
        style={{
          flexDirection: "row",
          flexWrap: "wrap",
          borderTopWidth: 1,
          borderTopColor: semantic.hairSoft,
          backgroundColor: semantic.hairSoft,
          opacity: busy ? 0.5 : 1,
        }}
        pointerEvents={busy ? "none" : "auto"}
      >
        {keys.map((k, i) => (
          <Pressable
            key={i}
            disabled={k === ""}
            onPress={() => (k === "bk" ? setPin(pin.slice(0, -1)) : push(k))}
            style={{
              width: "33.333%",
              backgroundColor: palette.bg,
              paddingVertical: 18,
              alignItems: "center",
              justifyContent: "center",
              gap: 2,
              marginBottom: 1,
            }}
          >
            <Text
              style={{
                fontFamily: fonts.sora400,
                fontSize: k === "bk" ? 18 : 22,
                color: k === "bk" ? semantic.ink2 : semantic.ink,
              }}
            >
              {k === "bk" ? "⌫" : k}
            </Text>
            {k && k !== "bk" ? (
              <Text
                style={{
                  fontFamily: fonts.sora400,
                  fontSize: 9,
                  letterSpacing: 1.8,
                  color: semantic.mute,
                }}
              >
                {SUBS[Number(k) - 1] || " "}
              </Text>
            ) : (
              <Text style={{ fontSize: 9 }}> </Text>
            )}
          </Pressable>
        ))}
      </View>
    </Screen>
  );
}
