import { useEffect, useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
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

type Mode = "chooser" | "polling" | "recovery";

export default function Enrollment() {
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
        onError: (e) => setError((e as Error).message || "Couldn't finalize."),
      });
    }
    if (status.data?.status === "rejected") {
      setError("The other device rejected this request.");
    }
    if (status.data?.status === "expired") {
      setError("Request expired. Start again.");
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
      onError: (e) => setError((e as Error).message || "Couldn't start enrollment."),
    });
  };

  const onRecover = () => {
    setError(null);
    if (!secretKey.trim()) {
      return;
    }
    recover.mutate(secretKey.trim(), {
      onSuccess: () => router.replace("/(auth)/pin"),
      onError: (e) => setError((e as Error).message || "Couldn't recover with that key."),
    });
  };

  return (
    <Screen testID="screen-auth-enrollment" centered>
      <Crumb segs={[{ label: "AUTH" }, { label: "Pair device", leaf: true }]} />
      <Body>
        <View style={{ paddingHorizontal: 24, paddingTop: 24, gap: 18 }}>
          <View style={{ gap: 8 }}>
            <Text style={[ty.h1, { color: semantic.ink }]}>
              {mode === "polling"
                ? "Approve on your other device"
                : mode === "recovery"
                  ? "Enter recovery key"
                  : "Pair this device"}
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
                ? "On a device that's already signed in, open Self → Security and approve the request with the code below."
                : mode === "recovery"
                  ? "Paste the recovery key you saved when you first signed up. It looks like a long string of letters and numbers."
                  : "This device doesn't have your keys yet. Choose how to get them onto it."}
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
                {start.isPending ? "STARTING…" : "APPROVE FROM ANOTHER DEVICE"}
              </Button>
              <Button
                testID="btn-enroll-recovery"
                full
                align="left"
                onPress={() => setMode("recovery")}
                icon={<Icon.key color={semantic.ink} />}
              >
                USE RECOVERY KEY
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
                  VERIFICATION CODE
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
                Waiting for approval…
              </Text>
            </View>
          ) : null}

          {mode === "recovery" ? (
            <View style={{ gap: 10, paddingTop: 6 }}>
              <Text style={ty.label}>RECOVERY KEY</Text>
              <Field
                testID="input-recovery-key"
                accessibilityLabel="Recovery key"
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
            {recover.isPending ? "RECOVERING…" : "RECOVER"}
          </Button>
          <Button
            testID="btn-enroll-back"
            variant="subtle"
            full
            onPress={() => setMode("chooser")}
          >
            Back
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
            Cancel
          </Button>
        </BottomAction>
      ) : null}
    </Screen>
  );
}
