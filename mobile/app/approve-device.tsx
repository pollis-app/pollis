import { useState } from "react";
import { ScrollView, Text, View } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { Trans, useTranslation } from "react-i18next";
import { Screen, Header, Button, Field, BottomAction } from "../components/ui";
import { ErrorText } from "../components/self/SettingsField";
import { semantic, type as ty, fonts, space } from "../theme/tokens";
import { useApproveEnrollment, useRejectEnrollment } from "../hooks/queries";
import { SAS_LENGTH, normalizeSasInput } from "../lib/enrollmentSas";
import { useSubmitLock } from "../hooks/useSubmitLock";
import { useThemeRerender } from "../components/theme";

/**
 * A new device of this account asks to be approved (#1360) — mobile's twin of
 * desktop's `EnrollmentApprovalPrompt`, opened over whatever is on screen by
 * `hooks/useEnrollmentApprovals`. Backing out leaves the request listed under
 * Self → Security.
 *
 * The approver TYPES the code shown on the new device (#1096). Nothing here
 * shows it: Rust compares what was typed against the code it derives from the
 * ephemeral key it fetched itself, so a server that swapped that key cannot
 * also supply the matching code.
 */
export default function ApproveDevice() {
  useThemeRerender();
  const { t } = useTranslation("auth");
  const router = useRouter();
  const { requestId, newDeviceId } = useLocalSearchParams<{
    requestId?: string;
    newDeviceId?: string;
  }>();
  const approve = useApproveEnrollment();
  const reject = useRejectEnrollment();
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  // One lock for both choices: approving wraps the account key for the new
  // device, and neither may run twice or race the other.
  const submit = useSubmitLock();
  const busy = submit.busy;
  const [acting, setActing] = useState<"approve" | "reject">("approve");
  const deviceId = newDeviceId ?? "";
  const shortDeviceId =
    deviceId.length > 12 ? `${deviceId.slice(0, 6)}…${deviceId.slice(-4)}` : deviceId;

  const onApprove = () => {
    if (!requestId || code.length !== SAS_LENGTH || !submit.begin()) {
      return;
    }
    setActing("approve");
    setError(null);
    approve.mutate(
      { requestId, verificationCode: code },
      {
        onSuccess: () => router.back(),
        onError: (e) => {
          submit.end();
          // A wrong code is the signal to re-read the other screen, not to
          // nudge one character and retry.
          setCode("");
          setError((e as Error).message || t("approval.approveFailed"));
        },
      },
    );
  };

  const onReject = () => {
    if (!requestId || !submit.begin()) {
      return;
    }
    setActing("reject");
    setError(null);
    reject.mutate(requestId, {
      onSuccess: () => router.back(),
      onError: (e) => {
        submit.end();
        setError((e as Error).message || t("approval.rejectFailed"));
      },
    });
  };

  return (
    <Screen testID="screen-approve-device">
      <Header title={t("approval.title")} />
      <ScrollView
        style={{ flex: 1 }}
        keyboardShouldPersistTaps="handled"
        contentContainerStyle={{
          paddingHorizontal: space.xxl,
          paddingTop: space.xxl,
          paddingBottom: space.xxxl,
          gap: space.lg,
        }}
      >
        <Text style={ty.body}>
          <Trans
            t={t}
            i18nKey="approval.body"
            values={{ deviceId: shortDeviceId }}
            components={{ code: <Text style={{ fontFamily: fonts.mono400, color: semantic.text }} /> }}
          />
        </Text>
        <Text style={ty.secondary}>{t("approval.instruction")}</Text>
        <View style={{ gap: space.xs }}>
          <Text style={ty.section}>{t("approval.codeLabel")}</Text>
          <Field
            testID="input-approve-device-code"
            accessibilityLabel={t("approval.codeLabel")}
            value={code}
            onChangeText={(next) => {
              setError(null);
              setCode(normalizeSasInput(next));
            }}
            placeholder={"·".repeat(SAS_LENGTH)}
            editable={!busy}
            autoFocus
            autoCorrect={false}
            autoCapitalize="characters"
            style={{ fontFamily: fonts.mono400, letterSpacing: 2 }}
          />
          <Text style={ty.meta}>{t("approval.codeHint", { count: SAS_LENGTH })}</Text>
        </View>
        {error ? <ErrorText testID="text-approve-device-error">{error}</ErrorText> : null}
      </ScrollView>
      {/* Pinned above the keyboard, which the code field raises: on a small
          phone both choices must stay reachable without dismissing it. */}
      <BottomAction>
        <Button
          variant="primary"
          full
          testID="btn-approve-device"
          disabled={code.length !== SAS_LENGTH || busy}
          loading={busy && acting === "approve"}
          onPress={onApprove}
        >
          {busy && acting === "approve" ? t("approval.approving") : t("approval.approve")}
        </Button>
        <Button
          full
          testID="btn-reject-device"
          disabled={busy}
          loading={busy && acting === "reject"}
          onPress={onReject}
        >
          {t("approval.reject")}
        </Button>
      </BottomAction>
    </Screen>
  );
}
