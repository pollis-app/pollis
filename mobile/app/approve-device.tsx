import { useState } from "react";
import { ScrollView, View } from "react-native";
import { Text } from "../components/Text";
import { useLocalSearchParams, useRouter } from "expo-router";
import { Trans, useTranslation } from "react-i18next";
import { Screen, Header, Button, BottomAction } from "../components/ui";
import { SasCodeField } from "../components/SasCodeField";
import { ErrorText } from "../components/self/SettingsField";
import { semantic, type as ty, fonts, space } from "../theme/tokens";
import { useApproveEnrollment, useRejectEnrollment } from "../hooks/queries";
import { SAS_LENGTH } from "../lib/enrollmentSas";
import { useSubmitLock } from "../hooks/useSubmitLock";
import { useThemeRerender } from "../components/theme";

/** A new device asks to be approved (#1360). The approver TYPES its code (#1096); nothing here shows it. */
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
  // One lock for both choices: neither may run twice or race the other.
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
          // A wrong code means re-reading the other screen.
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

  // #1972: opened with no request (a stale link, a request already handled): say so, never "(id )".
  if (!requestId) {
    return (
      <Screen testID="screen-approve-device">
        <Header title={t("approval.title")} />
        <View style={{ flex: 1, paddingHorizontal: space.xxl, paddingTop: space.xxl }}>
          <Text testID="text-approve-device-none" style={ty.body}>
            {t("approval.none")}
          </Text>
        </View>
        <BottomAction>
          <Button
            full
            testID="btn-approve-device-back"
            onPress={() => (router.canGoBack() ? router.back() : router.replace("/"))}
          >
            {t("common:actions.back")}
          </Button>
        </BottomAction>
      </Screen>
    );
  }

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
        <Text testID="text-approve-device-body" style={ty.body}>
          {shortDeviceId ? (
            <Trans
              t={t}
              i18nKey="approval.body"
              values={{ deviceId: shortDeviceId }}
              components={{ code: <Text style={{ fontFamily: fonts.mono400, color: semantic.text }} /> }}
            />
          ) : (
            t("approval.bodyNoId")
          )}
        </Text>
        <Text style={ty.secondary}>{t("approval.instruction")}</Text>
        <View style={{ gap: space.xs }}>
          <Text style={ty.section}>{t("approval.codeLabel")}</Text>
          <SasCodeField
            testID="input-approve-device-code"
            accessibilityLabel={t("approval.codeLabel")}
            value={code}
            onChange={(next) => {
              setError(null);
              setCode(next);
            }}
            editable={!busy}
            autoFocus
          />
          <Text style={ty.meta}>{t("approval.codeHint", { count: SAS_LENGTH })}</Text>
        </View>
        {error ? <ErrorText testID="text-approve-device-error">{error}</ErrorText> : null}
      </ScrollView>
      {/* Pinned above the keyboard: both choices stay reachable. */}
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
          variant="danger"
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
