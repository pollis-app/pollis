import { useEffect, useRef, useState } from "react";
import { ActivityIndicator, View, Text } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Body, Card, Field, Button, BottomAction } from "../../components/ui";
import { Icon } from "../../components/icons";
import { Heading } from "../../components/auth/Heading";
import { AuthError } from "../../components/auth/AuthError";
import { semantic, type as ty, fonts } from "../../theme/tokens";
import {
  useStartEnrollment,
  useEnrollmentStatus,
  useRecoverWithSecretKey,
  type EnrollmentHandle,
} from "../../hooks/queries";

type Mode = "chooser" | "polling" | "recovery";

export default function Enrollment() {
  const { t } = useTranslation("mobile");
  const router = useRouter();
  // Signed in by a QR device link (#1207): no chooser and no code to read
  // out — the request starts at once and the device that showed the QR
  // approves it on the link tag.
  const { linked } = useLocalSearchParams<{ linked?: string }>();
  const isLinked = linked === "1";
  const autoStarted = useRef(false);
  const [mode, setMode] = useState<Mode>("chooser");
  const [handle, setHandle] = useState<EnrollmentHandle | null>(null);
  const [secretKey, setSecretKey] = useState("");
  const [error, setError] = useState<string | null>(null);

  const start = useStartEnrollment();
  const recover = useRecoverWithSecretKey();
  const status = useEnrollmentStatus(
    mode === "polling" ? handle?.request_id ?? null : null,
  );

  // When the existing device approves, go straight to PIN-create. Finalize
  // (publish this device's cert, external-join the account's groups) needs
  // the local DB, which only `set_pin` opens — so it runs on the PIN screen
  // after `set_pin`, exactly as desktop's handlePinCreated does. Calling it
  // here, before the PIN, failed every device-linking sign-in with "not
  // signed in for DS request signing".
  useEffect(() => {
    if (status.data?.status === "approved") {
      router.replace("/(auth)/pin");
    }
    if (status.data?.status === "rejected") {
      setError(t("auth.enrollment.rejected"));
    }
    if (status.data?.status === "expired") {
      setError(t("auth.enrollment.expired"));
    }
    // router is stable.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status.data?.status]);

  useEffect(() => {
    if (isLinked && !autoStarted.current) {
      autoStarted.current = true;
      onStart();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isLinked]);

  const onStart = () => {
    setError(null);
    start.mutate(undefined, {
      onSuccess: (h) => {
        setHandle(h);
        setMode("polling");
      },
      onError: (e) =>
        setError((e as Error).message || t("auth:enroll.startFailed")),
    });
  };

  const onRecover = () => {
    setError(null);
    if (!secretKey.trim()) {
      return;
    }
    recover.mutate(secretKey.trim(), {
      onSuccess: () => router.replace("/(auth)/pin"),
      onError: (e) =>
        setError((e as Error).message || t("auth:recover.failed")),
    });
  };

  const title =
    mode === "polling" && isLinked
      ? t("auth:link.awaitingTitle")
      : mode === "polling"
        ? t("auth.enrollment.pollingTitle")
        : mode === "recovery"
          ? t("auth.enrollment.recoveryTitle")
          : t("auth.enrollment.chooserTitle");
  const intro =
    mode === "polling" && isLinked
      ? t("auth:link.awaitingIntro")
      : mode === "polling"
        ? t("auth.enrollment.pollingIntro")
        : mode === "recovery"
          ? t("auth.enrollment.recoveryIntro")
          : t("auth.enrollment.chooserIntro");

  // "Waiting for approval…" with a spinner. The text stays one element: the
  // two-client device-link flow waits for it by its text.
  const waiting = (testID?: string) => (
    <View
      style={{ flexDirection: "row", alignItems: "center", justifyContent: "center", gap: 10 }}
      accessibilityLiveRegion="polite"
    >
      <ActivityIndicator color={semantic.accent} />
      <Text testID={testID} style={ty.secondary}>
        {t("auth.enrollment.waiting")}
      </Text>
    </View>
  );

  return (
    <Screen testID="screen-auth-enrollment" centered>
      <Body contentContainerStyle={{ paddingHorizontal: 24, paddingTop: 52, gap: 24 }}>
        <Heading title={title} subtitle={intro} />

        {mode === "chooser" ? (
          <View style={{ gap: 10 }}>
            <Button
              testID="btn-enroll-approve-device"
              full
              variant="primary"
              onPress={onStart}
              disabled={start.isPending}
              icon={<Icon.device size={18} color={semantic.onAccent} />}
            >
              {start.isPending
                ? t("auth.enrollment.starting")
                : t("auth:enroll.approveFromDevice")}
            </Button>
            <Button
              testID="btn-enroll-recovery"
              full
              variant="secondary"
              onPress={() => setMode("recovery")}
              icon={<Icon.key size={18} color={semantic.text} />}
            >
              {t("auth.enrollment.useRecoveryKey")}
            </Button>
          </View>
        ) : null}

        {mode === "polling" && handle && isLinked ? waiting("linked-awaiting-approval") : null}

        {mode === "polling" && handle && !isLinked ? (
          <View style={{ gap: 20 }}>
            <Card
              style={{
                alignItems: "center",
                gap: 10,
                paddingVertical: 24,
              }}
            >
              <Text style={ty.section}>{t("auth.enrollment.verificationCode")}</Text>
              {/* Exactly the 8 code characters in ONE Text, with no
                  accessibilityLabel of its own: the two-client device-link
                  script reads it off the accessibility tree. */}
              <Text
                selectable
                style={{
                  fontFamily: fonts.mono500,
                  fontSize: 34,
                  lineHeight: 42,
                  letterSpacing: 4,
                  color: semantic.text,
                  textAlign: "center",
                }}
              >
                {handle.verification_code}
              </Text>
            </Card>
            {waiting()}
          </View>
        ) : null}

        {mode === "recovery" ? (
          <View style={{ gap: 8 }}>
            <Text style={ty.section}>{t("auth.enrollment.recoveryKeyLabel")}</Text>
            <Field
              testID="input-recovery-key"
              accessibilityLabel={t("auth.enrollment.recoveryKeyLabel")}
              value={secretKey}
              onChangeText={setSecretKey}
              autoCorrect={false}
              icon={<Icon.key size={18} color={semantic.muted} />}
              style={{ fontFamily: fonts.mono400 }}
            />
          </View>
        ) : null}

        {error ? <AuthError message={error} /> : null}
      </Body>
      {mode === "recovery" ? (
        <BottomAction>
          <Button
            testID="btn-submit-recovery"
            full
            variant="primary"
            onPress={onRecover}
            disabled={!secretKey.trim() || recover.isPending}
            iconRight={<Icon.arrowRight size={18} color={semantic.onAccent} />}
          >
            {recover.isPending
              ? t("auth:recover.recovering")
              : t("auth.enrollment.recover")}
          </Button>
          <Button
            testID="btn-enroll-back"
            variant="subtle"
            full
            onPress={() => setMode("chooser")}
          >
            {t("common:actions.back")}
          </Button>
        </BottomAction>
      ) : mode === "polling" ? (
        <BottomAction>
          <Button
            testID="btn-enroll-cancel"
            variant="subtle"
            full
            onPress={() => {
              setHandle(null);
              setMode("chooser");
            }}
          >
            {t("common:actions.cancel")}
          </Button>
        </BottomAction>
      ) : null}
    </Screen>
  );
}
