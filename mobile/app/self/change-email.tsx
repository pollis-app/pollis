import { useState } from "react";
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
  Ctx,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { upper } from "../../i18n";
import { useMutation } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

type Stage = "enter-email" | "enter-code";

function ChangeEmail() {
  const { t } = useTranslation("settings");
  const router = useRouter();
  const currentUser = appStore.currentUser;
  const setCurrentUser = appStore.setCurrentUser;
  const [stage, setStage] = useState<Stage>("enter-email");
  const [newEmail, setNewEmail] = useState("");
  const [code, setCode] = useState("");
  // The second proof (#1161): a code to the address the account is on today.
  // The device signature and the new-address code are both satisfied by whoever
  // is holding this phone unlocked, so without this one a borrowed device could
  // move the account's recovery address.
  const [currentCode, setCurrentCode] = useState("");

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
        currentCode: currentCode.trim(),
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
      if (code.trim().length === 0 || currentCode.trim().length === 0) {
        return;
      }
      verify.mutate();
    }
  };

  const pending = requestOtp.isPending || verify.isPending;
  const error = requestOtp.error ?? verify.error;

  return (
    <Screen testID="screen-self-change-email" centered>
      <Crumb
        segs={[
          { label: upper(t("mobile:self.title")) },
          { label: t("user.title") },
          { label: t("user.emailLabel"), leaf: true },
        ]}
      />
      <Body>
        <View style={{ paddingHorizontal: 18, paddingTop: 14, gap: 14 }}>
          <Text style={[ty.h1, { color: semantic.ink }]}>
            {t("mobile:self.changeEmail.title")}
          </Text>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              lineHeight: 19,
              color: semantic.mute,
            }}
          >
            {stage === "enter-email"
              ? t("mobile:self.changeEmail.enterEmailIntro")
              : t("mobile:self.changeEmail.enterCodeIntro", { email: newEmail })}
          </Text>

          {stage === "enter-email" ? (
            <View style={{ gap: 6 }}>
              <Text style={ty.label}>{upper(t("user.newEmailLabel"))}</Text>
              <Field
                amber
                value={newEmail}
                onChangeText={setNewEmail}
                testID="input-email"
                accessibilityLabel={t("user.newEmailLabel")}
                icon={<Icon.mail color={semantic.mute} />}
                keyboardType="email-address"
              />
            </View>
          ) : (
            <View style={{ gap: 14 }}>
              <View style={{ gap: 6 }}>
                <Text style={ty.label}>
                  {upper(
                    t("mobile:self.changeEmail.newCodeLabel", {
                      email: newEmail.trim(),
                    }),
                  )}
                </Text>
                <Field
                  amber
                  value={code}
                  onChangeText={(v) =>
                    setCode(v.replace(/[^0-9]/g, "").slice(0, 6))
                  }
                  testID="input-otp"
                  accessibilityLabel={t("user.verificationCodeLabel")}
                  keyboardType="number-pad"
                  icon={<Icon.key color={semantic.mute} />}
                />
              </View>
              <View style={{ gap: 6 }}>
                <Text style={ty.label}>
                  {upper(
                    t("mobile:self.changeEmail.currentCodeLabel", {
                      email: currentUser?.email ?? "",
                    }),
                  )}
                </Text>
                <Field
                  amber
                  value={currentCode}
                  onChangeText={(v) =>
                    setCurrentCode(v.replace(/[^0-9]/g, "").slice(0, 6))
                  }
                  testID="input-current-otp"
                  accessibilityLabel={t("user.currentCodeLabel")}
                  keyboardType="number-pad"
                  icon={<Icon.key color={semantic.mute} />}
                />
              </View>
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
              {(error as Error).message || t("errors:boundary.title")}
            </Text>
          ) : null}
        </View>
      </Body>
      <Ctx
        cr={upper(t("mobile:self.title"))}
        name={t("mobile:self.changeEmail.title")}
      />
      <BottomAction>
        <Button
          full
          testID={stage === "enter-email" ? "btn-request-otp" : "btn-submit"}
          variant="primary"
          onPress={onSubmit}
          disabled={
            pending ||
            (stage === "enter-email"
              ? !newEmail.trim()
              : code.trim().length !== 6 || currentCode.trim().length !== 6)
          }
          iconRight={<Icon.arrowRight color="#0a0907" />}
        >
          {pending
            ? upper(t("mobile:common.working"))
            : stage === "enter-email"
              ? upper(t("user.sendCodeButton"))
              : upper(t("mobile:self.changeEmail.confirm"))}
        </Button>
        {stage === "enter-code" ? (
          <Button
            variant="subtle"
            full
            testID="btn-use-different-email"
            onPress={() => {
              setCode("");
              setCurrentCode("");
              setStage("enter-email");
            }}
          >
            {t("mobile:self.changeEmail.useDifferentEmail")}
          </Button>
        ) : null}
      </BottomAction>
    </Screen>
  );
}

export default observer(ChangeEmail);
