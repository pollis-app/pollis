import { useEffect, useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Crumb,
  Body,
  Field,
  Button,
  BottomAction,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty, fonts } from "../../theme/tokens";
import {
  useStartEnrollment,
  useEnrollmentStatus,
  useFinalizeEnrollment,
  useRecoverWithSecretKey,
  type EnrollmentHandle,
} from "../../hooks/queries";
import { upper } from "../../i18n";

type Mode = "chooser" | "polling" | "recovery";

export default function Enrollment() {
  const { t } = useTranslation("mobile");
  const router = useRouter();
  const [mode, setMode] = useState<Mode>("chooser");
  const [handle, setHandle] = useState<EnrollmentHandle | null>(null);
  const [secretKey, setSecretKey] = useState("");
  const [error, setError] = useState<string | null>(null);

  const start = useStartEnrollment();
  const finalize = useFinalizeEnrollment();
  const recover = useRecoverWithSecretKey();
  const status = useEnrollmentStatus(
    mode === "polling" ? handle?.request_id ?? null : null,
  );

  // When the existing device approves, finalize on this side then route
  // to PIN-create so the user can set a local PIN for this device.
  useEffect(() => {
    if (status.data?.status === "approved") {
      finalize.mutate(undefined, {
        onSuccess: () => router.replace("/(auth)/pin"),
        onError: (e) =>
          setError((e as Error).message || t("auth.enrollment.finalizeFailed")),
      });
    }
    if (status.data?.status === "rejected") {
      setError(t("auth.enrollment.rejected"));
    }
    if (status.data?.status === "expired") {
      setError(t("auth.enrollment.expired"));
    }
    // finalize is a stable mutation ref.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status.data?.status]);

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

  return (
    <Screen testID="screen-auth-enrollment" centered>
      <Crumb
        segs={[
          { label: upper(t("auth.crumb.auth")) },
          { label: t("auth.crumb.pairDevice"), leaf: true },
        ]}
      />
      <Body>
        <View style={{ paddingHorizontal: 24, paddingTop: 24, gap: 18 }}>
          <View style={{ gap: 8 }}>
            <Text style={[ty.h1, { color: semantic.ink }]}>
              {mode === "polling"
                ? t("auth.enrollment.pollingTitle")
                : mode === "recovery"
                  ? t("auth.enrollment.recoveryTitle")
                  : t("auth.enrollment.chooserTitle")}
            </Text>
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 13,
                lineHeight: 19,
                color: semantic.mute,
              }}
            >
              {mode === "polling"
                ? t("auth.enrollment.pollingIntro")
                : mode === "recovery"
                  ? t("auth.enrollment.recoveryIntro")
                  : t("auth.enrollment.chooserIntro")}
            </Text>
          </View>

          {mode === "chooser" ? (
            <View style={{ gap: 10, paddingTop: 6 }}>
              <Button
                testID="btn-enroll-approve-device"
                full
                align="left"
                variant="primary"
                onPress={onStart}
                disabled={start.isPending}
                icon={<Icon.device color="#0a0907" />}
              >
                {upper(
                  start.isPending
                    ? t("auth.enrollment.starting")
                    : t("auth:enroll.approveFromDevice"),
                )}
              </Button>
              <Button
                testID="btn-enroll-recovery"
                full
                align="left"
                onPress={() => setMode("recovery")}
                icon={<Icon.key color={semantic.ink} />}
              >
                {upper(t("auth.enrollment.useRecoveryKey"))}
              </Button>
            </View>
          ) : null}

          {mode === "polling" && handle ? (
            <View style={{ gap: 14, paddingTop: 6 }}>
              <View
                style={{
                  borderWidth: 1,
                  borderColor: semantic.accent,
                  backgroundColor: semantic.accentSoft,
                  paddingVertical: 18,
                  paddingHorizontal: 14,
                  alignItems: "center",
                }}
              >
                <Text style={[ty.label, { marginBottom: 6 }]}>
                  {upper(t("auth.enrollment.verificationCode"))}
                </Text>
                <Text
                  style={{
                    fontFamily: fonts.mono400,
                    fontSize: 28,
                    letterSpacing: 4,
                    color: semantic.ink,
                  }}
                >
                  {handle.verification_code}
                </Text>
              </View>
              <Text
                style={{
                  fontFamily: ty.body.fontFamily,
                  fontSize: 12,
                  color: semantic.mute,
                  textAlign: "center",
                }}
              >
                {t("auth.enrollment.waiting")}
              </Text>
            </View>
          ) : null}

          {mode === "recovery" ? (
            <View style={{ gap: 10, paddingTop: 6 }}>
              <Text style={ty.label}>
                {upper(t("auth.enrollment.recoveryKeyLabel"))}
              </Text>
              <Field
                testID="input-recovery-key"
                accessibilityLabel={t("auth.enrollment.recoveryKeyLabel")}
                amber
                value={secretKey}
                onChangeText={setSecretKey}
                icon={<Icon.key color={semantic.mute} />}
              />
            </View>
          ) : null}

          {error ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.danger,
              }}
            >
              {error}
            </Text>
          ) : null}
        </View>
      </Body>
      {mode === "recovery" ? (
        <BottomAction>
          <Button
            testID="btn-submit-recovery"
            full
            variant="primary"
            onPress={onRecover}
            disabled={!secretKey.trim() || recover.isPending}
            iconRight={<Icon.arrowRight color="#0a0907" />}
          >
            {upper(
              recover.isPending
                ? t("auth:recover.recovering")
                : t("auth.enrollment.recover"),
            )}
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
