import { useEffect, useRef, useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Card, Button } from "../../components/ui";
import { Icon } from "../../components/icons";
import { AuthError } from "../../components/auth/AuthError";
import { DotField } from "../../components/auth/DotField";
import { ProgressBar } from "../../components/auth/ProgressBar";
import { StepRow, type StepState } from "../../components/auth/StepRow";
import { semantic, type as ty } from "../../theme/tokens";
import { useQuery } from "@tanstack/react-query";
import { useInitializeIdentity } from "../../hooks/queries/useAuth";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

interface Step {
  n: string;
  s: string;
  state: StepState;
}

function Initializing() {
  const { t } = useTranslation("mobile");
  const router = useRouter();
  const currentUser = appStore.currentUser;
  const initIdentity = useInitializeIdentity();
  const [error, setError] = useState<string | null>(null);

  // Real core version from the bridge's `version` command (pollis-core's
  // CARGO_PKG_VERSION) — replaces the design handoff's hardcoded string.
  const { data: coreVersion } = useQuery({
    queryKey: ["core-version"],
    queryFn: () => invoke<string>("version"),
    staleTime: Infinity,
  });

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
      onError: (e) =>
        setError((e as Error).message || t("auth.initializing.setupFailed")),
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
    ? t("auth.initializing.working")
    : initIdentity.isSuccess
      ? t("settings:changePin.doneButton")
      : t("auth.initializing.ready");
  const ok = t("auth.initializing.statusOk");
  const steps: Step[] = [
    { n: t("auth.initializing.stepKeysLoaded"), s: ok, state: "done" },
    { n: t("auth.initializing.stepDevicePaired"), s: ok, state: "done" },
    {
      n: t("auth.initializing.stepInitializeIdentity"),
      s: initIdentity.isPending
        ? "…"
        : initIdentity.isSuccess
          ? ok
          : initIdentity.isError
            ? t("auth.initializing.statusError")
            : "—",
      state: initIdentity.isSuccess
        ? "done"
        : initIdentity.isError
          ? "error"
          : initIdentity.isPending
            ? "active"
            : "todo",
    },
    { n: t("auth.initializing.stepResolvePeers"), s: "—", state: "todo" },
  ];
  const percent = initIdentity.isSuccess ? 100 : initIdentity.isPending ? 62 : 30;

  return (
    <Screen testID="screen-auth-initializing" centered>
      <DotField />

      <View
        style={{
          flex: 1,
          justifyContent: "center",
          paddingHorizontal: 24,
        }}
      >
        <Card
          style={{
            width: "100%",
            borderWidth: 1,
            borderColor: semantic.edge,
            padding: 20,
            gap: 20,
          }}
        >
          <View style={{ gap: 6 }}>
            <Text accessibilityRole="header" style={ty.title}>
              {t("auth.initializing.title")}
            </Text>
            <Text style={ty.secondary}>{t("auth.initializing.intro")}</Text>
          </View>

          <View style={{ gap: 8 }}>
            <Text accessibilityLiveRegion="polite" style={ty.section}>
              {progress}
            </Text>
            <ProgressBar
              percent={percent}
              indeterminate={initIdentity.isPending}
              label={progress}
            />
          </View>

          <View style={{ gap: 4 }}>
            {steps.map((step, i) => (
              <StepRow key={i} name={step.n} status={step.s} state={step.state} />
            ))}
          </View>

          {error ? <AuthError message={error} /> : null}
        </Card>
      </View>

      <View
        style={{
          flexDirection: "row",
          justifyContent: "space-between",
          alignItems: "center",
          paddingStart: 24,
          paddingEnd: 8,
          paddingVertical: 8,
        }}
      >
        <Text style={ty.meta}>{coreVersion ? `v${coreVersion}` : ""}</Text>
        <Button
          testID="btn-continue"
          variant="subtle"
          onPress={() => router.replace("/(tabs)/groups")}
          iconRight={<Icon.arrowRight size={16} color={semantic.text} />}
        >
          {t("auth.initializing.skip")}
        </Button>
      </View>
    </Screen>
  );
}

export default observer(Initializing);
