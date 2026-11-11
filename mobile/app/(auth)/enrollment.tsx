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
  useAwaitEnrollmentApproval,
  useRecoverWithSecretKey,
  type EnrollmentHandle,
  noteEnrollmentApprover,
} from "../../hooks/queries";
import { useSubmitLock } from "../../hooks/useSubmitLock";
import { isEnrollmentRequestLive } from "../../lib/enrollmentWait";
import { useThemeRerender } from "../../components/theme";

type Mode = "chooser" | "waiting" | "recovery";

export default function Enrollment() {
  useThemeRerender();
  const { t } = useTranslation("mobile");
  const router = useRouter();
  // Signed in by a QR device link (#1207): no chooser, no code; the request starts at once.
  const { linked } = useLocalSearchParams<{ linked?: string }>();
  const isLinked = linked === "1";
  const autoStarted = useRef(false);
  const [mode, setMode] = useState<Mode>("chooser");
  const [handle, setHandle] = useState<EnrollmentHandle | null>(null);
  const [secretKey, setSecretKey] = useState("");
  const [error, setError] = useState<string | null>(null);

  const start = useStartEnrollment();
  const recover = useRecoverWithSecretKey();
  // Slow, and must not run twice.
  const recovering = useSubmitLock();
  const starting = useSubmitLock();
  // One awaited call per request, never a poll (#1809); it ends on an answer, an error or the deadline.
  const wait = useAwaitEnrollmentApproval(mode === "waiting" ? handle : null);
  const outcome = wait.outcome;

  // On approval go to PIN-create; finalize needs the local DB, which only `set_pin` opens.
  useEffect(() => {
    if (outcome?.kind === "approved") {
      noteEnrollmentApprover(outcome.approvedByDeviceId);
      router.replace("/(auth)/pin");
    }
    if (outcome?.kind === "error") {
      console.warn("[enrollment] approval wait failed:", outcome.message);
    }
    // router is stable.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [outcome]);

  // The wait ended without an approval: say why and offer Try again. Nothing retries on its own.
  // A failed check reads in plain words; the core's error goes to the log, not the screen.
  const waitError =
    outcome?.kind === "rejected"
      ? t("auth.enrollment.rejected")
      : outcome?.kind === "expired"
        ? t("auth.enrollment.expired")
        : outcome?.kind === "error"
          ? t("auth.enrollment.waitFailed")
          : null;
  const waitEnded = waitError !== null;
  // Rejected or expired: the code on screen is dead. After a failed check it still stands.
  const requestOver = outcome?.kind === "rejected" || outcome?.kind === "expired";
  const shownError = error ?? (mode === "waiting" ? waitError : null);

  useEffect(() => {
    if (isLinked && !autoStarted.current) {
      autoStarted.current = true;
      onStart();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isLinked]);

  const onStart = () => {
    if (!starting.begin()) {
      return;
    }
    setError(null);
    start.mutate(undefined, {
      onSuccess: (h) => {
        setHandle(h);
        setMode("waiting");
      },
      onError: (e) =>
        setError((e as Error).message || t("auth:enroll.startFailed")),
      onSettled: starting.end,
    });
  };

  // A failed await on a request that is still open waits on it again; anything else needs a new request.
  const onTryAgain = () => {
    if (
      outcome?.kind === "error" &&
      handle &&
      isEnrollmentRequestLive(handle.expires_at, Date.now())
    ) {
      setError(null);
      wait.retry();
      return;
    }
    onStart();
  };

  const onRecover = () => {
    setError(null);
    if (!secretKey.trim() || !recovering.begin()) {
      return;
    }
    recover.mutate(secretKey.trim(), {
      onSuccess: () => router.replace("/(auth)/pin"),
      onError: (e) => {
        recovering.end();
        setError((e as Error).message || t("auth:recover.failed"));
      },
    });
  };

  const title =
    mode === "waiting" && isLinked
      ? t("auth:link.awaitingTitle")
      : mode === "waiting"
        ? t("auth.enrollment.pollingTitle")
        : mode === "recovery"
          ? t("auth.enrollment.recoveryTitle")
          : t("auth.enrollment.chooserTitle");
  const intro =
    mode === "waiting" && isLinked
      ? t("auth:link.awaitingIntro")
      : mode === "waiting"
        ? t("auth.enrollment.pollingIntro")
        : mode === "recovery"
          ? t("auth.enrollment.recoveryIntro")
          : t("auth.enrollment.chooserIntro");

  // One text element: the device-link flow waits for it by its text.
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
              loading={starting.busy}
              icon={<Icon.device size={18} color={semantic.onAccent} />}
            >
              {starting.busy
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

        {mode === "waiting" && handle && isLinked && !waitEnded
          ? waiting("linked-awaiting-approval")
          : null}

        {mode === "waiting" && handle && !isLinked && !requestOver ? (
          <View style={{ gap: 20 }}>
            <Card
              style={{
                alignItems: "center",
                gap: 10,
                paddingVertical: 24,
              }}
            >
              <Text style={ty.section}>{t("auth.enrollment.verificationCode")}</Text>
              {/* Exactly the 8 code characters in ONE Text: the device-link script reads it. */}
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
            {waitEnded ? null : waiting()}
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

        {shownError ? <AuthError testID="text-enroll-error" message={shownError} /> : null}
      </Body>
      {mode === "recovery" ? (
        <BottomAction>
          <Button
            testID="btn-submit-recovery"
            full
            variant="primary"
            onPress={onRecover}
            disabled={!secretKey.trim()}
            loading={recovering.busy}
            iconRight={<Icon.arrowRight size={18} color={semantic.onAccent} />}
          >
            {recovering.busy
              ? t("auth:recover.recovering")
              : t("auth.enrollment.recover")}
          </Button>
          <Button
            testID="btn-enroll-back"
            variant="subtle"
            full
            disabled={recovering.busy}
            onPress={() => setMode("chooser")}
          >
            {t("common:actions.back")}
          </Button>
        </BottomAction>
      ) : mode === "waiting" ? (
        <BottomAction>
          {waitEnded ? (
            <Button
              testID="btn-enroll-try-again"
              full
              variant="primary"
              onPress={onTryAgain}
              loading={starting.busy}
            >
              {t("auth:enroll.tryAgain")}
            </Button>
          ) : null}
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
