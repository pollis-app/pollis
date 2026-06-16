import { useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  Field,
  Button,
  BottomAction,
  Ctx,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { useMutation } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

type Stage = "enter-email" | "enter-code";

function ChangeEmail() {
  const router = useRouter();
  const currentUser = appStore.currentUser;
  const setCurrentUser = appStore.setCurrentUser;
  const [stage, setStage] = useState<Stage>("enter-email");
  const [newEmail, setNewEmail] = useState("");
  const [code, setCode] = useState("");

  const requestOtp = useMutation({
    mutationFn: async (email: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("request_email_change_otp", {
        userId: currentUser.id,
        newEmail: email,
      });
    },
    onSuccess: () => setStage("enter-code"),
  });

  const verify = useMutation({
    mutationFn: async () => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("verify_email_change", {
        userId: currentUser.id,
        newEmail: newEmail.trim(),
        code: code.trim(),
      });
    },
    onSuccess: () => {
      if (currentUser) {
        setCurrentUser({ ...currentUser, email: newEmail.trim() });
      }
      router.back();
    },
  });

  const onSubmit = () => {
    if (stage === "enter-email") {
      const trimmed = newEmail.trim();
      if (!trimmed) {
        return;
      }
      requestOtp.mutate(trimmed);
    } else {
      if (code.trim().length === 0) {
        return;
      }
      verify.mutate();
    }
  };

  const pending = requestOtp.isPending || verify.isPending;
  const error = requestOtp.error ?? verify.error;

  return (
    <Screen>
      <Crumb
        segs={[
          { label: "SELF" },
          { label: "User settings" },
          { label: "Email", leaf: true },
        ]}
      />
      <Body>
        <View style={{ paddingHorizontal: 18, paddingTop: 14, gap: 14 }}>
          <Text style={[ty.h1, { color: semantic.ink }]}>Change email</Text>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              lineHeight: 19,
              color: semantic.mute,
            }}
          >
            {stage === "enter-email"
              ? "Enter the new email. We'll send a 6-digit code to confirm you own it."
              : `Enter the code we sent to ${newEmail}.`}
          </Text>

          {stage === "enter-email" ? (
            <View style={{ gap: 6 }}>
              <Text style={ty.label}>NEW EMAIL</Text>
              <Field
                amber
                value={newEmail}
                onChangeText={setNewEmail}
                icon={<Icon.mail color={semantic.mute} />}
                keyboardType="email-address"
              />
            </View>
          ) : (
            <View style={{ gap: 6 }}>
              <Text style={ty.label}>VERIFICATION CODE</Text>
              <Field
                amber
                value={code}
                onChangeText={(v) =>
                  setCode(v.replace(/[^0-9]/g, "").slice(0, 6))
                }
                keyboardType="number-pad"
                icon={<Icon.key color={semantic.mute} />}
              />
            </View>
          )}

          {error ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.danger,
              }}
            >
              {(error as Error).message || "Something went wrong."}
            </Text>
          ) : null}
        </View>
      </Body>
      <Ctx cr="SELF" name="Change email" />
      <BottomAction>
        <Button
          full
          variant="primary"
          onPress={onSubmit}
          disabled={
            pending ||
            (stage === "enter-email"
              ? !newEmail.trim()
              : code.trim().length !== 6)
          }
          iconRight={<Icon.arrowRight color="#0a0907" />}
        >
          {pending
            ? "WORKING…"
            : stage === "enter-email"
              ? "SEND CODE"
              : "CONFIRM"}
        </Button>
        {stage === "enter-code" ? (
          <Button
            variant="subtle"
            full
            onPress={() => {
              setCode("");
              setStage("enter-email");
            }}
          >
            Use a different email
          </Button>
        ) : null}
      </BottomAction>
    </Screen>
  );
}

export default observer(ChangeEmail);
