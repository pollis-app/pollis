import { useRef, useState } from "react";
import { ScrollView, Text, TextInput, View } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { useMutation } from "@tanstack/react-query";
import { observer } from "mobx-react-lite";
import { Screen, Header, Field, Button, BottomAction } from "../../components/ui";
import { SettingsField, ErrorText } from "../../components/self/SettingsField";
import { Icon } from "../../components/icons";
import { semantic, type as ty, space } from "../../theme/tokens";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";

type Stage = "enter-email" | "enter-code";

const CODE_LENGTH = 6;

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
  const scrollRef = useRef<ScrollView>(null);
  const currentCodeRef = useRef<TextInput>(null);

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

  // #1231: with the number pad up, the second code field could sit behind the
  // keyboard. Once the first code is complete, move focus on to the second and
  // scroll it into view.
  const onCodeChange = (v: string) => {
    const next = v.replace(/[^0-9]/g, "").slice(0, CODE_LENGTH);
    setCode(next);
    if (next.length === CODE_LENGTH && currentCode.length < CODE_LENGTH) {
      currentCodeRef.current?.focus();
      requestAnimationFrame(() => scrollRef.current?.scrollToEnd({ animated: true }));
    }
  };

  const pending = requestOtp.isPending || verify.isPending;
  const error = requestOtp.error ?? verify.error;

  return (
    <Screen testID="screen-self-change-email" centered>
      <Header
        title={t("mobile:self.changeEmail.title")}
        backTo={t("mobile:self.hub.accountDetails")}
      />
      <ScrollView
        ref={scrollRef}
        style={{ flex: 1 }}
        keyboardShouldPersistTaps="handled"
        contentContainerStyle={{
          paddingHorizontal: space.xxl,
          paddingTop: space.xxl,
          paddingBottom: space.xxxl,
          gap: space.xxl,
        }}
      >
        <Text style={ty.secondary}>
          {stage === "enter-email"
            ? t("mobile:self.changeEmail.enterEmailIntro")
            : t("mobile:self.changeEmail.enterCodeIntro", { email: newEmail })}
        </Text>

        {stage === "enter-email" ? (
          <SettingsField label={t("user.newEmailLabel")}>
            <Field
              value={newEmail}
              onChangeText={setNewEmail}
              testID="input-email"
              accessibilityLabel={t("user.newEmailLabel")}
              icon={<Icon.mail size={18} color={semantic.muted} />}
              keyboardType="email-address"
              autoComplete="email"
            />
          </SettingsField>
        ) : (
          <View style={{ gap: space.xxl }}>
            <SettingsField
              label={t("mobile:self.changeEmail.newCodeLabel", { email: newEmail.trim() })}
            >
              <Field
                value={code}
                onChangeText={onCodeChange}
                testID="input-otp"
                accessibilityLabel={t("user.verificationCodeLabel")}
                keyboardType="number-pad"
                textContentType="oneTimeCode"
                icon={<Icon.key size={18} color={semantic.muted} />}
              />
            </SettingsField>
            <SettingsField
              label={t("mobile:self.changeEmail.currentCodeLabel", {
                email: currentUser?.email ?? "",
              })}
            >
              <Field
                ref={currentCodeRef}
                value={currentCode}
                onChangeText={(v) =>
                  setCurrentCode(v.replace(/[^0-9]/g, "").slice(0, CODE_LENGTH))
                }
                onFocus={() =>
                  requestAnimationFrame(() => scrollRef.current?.scrollToEnd({ animated: true }))
                }
                testID="input-current-otp"
                accessibilityLabel={t("user.currentCodeLabel")}
                keyboardType="number-pad"
                icon={<Icon.key size={18} color={semantic.muted} />}
              />
            </SettingsField>
            <View style={{ alignItems: "flex-start" }}>
              <Button
                variant="subtle"
                testID="btn-use-different-email"
                onPress={() => {
                  setCode("");
                  setCurrentCode("");
                  setStage("enter-email");
                }}
              >
                {t("mobile:self.changeEmail.useDifferentEmail")}
              </Button>
            </View>
          </View>
        )}

        {error ? (
          <ErrorText>{(error as Error).message || t("errors:boundary.title")}</ErrorText>
        ) : null}
      </ScrollView>
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
              : code.trim().length !== CODE_LENGTH ||
                currentCode.trim().length !== CODE_LENGTH)
          }
          iconRight={<Icon.arrowRight size={18} color={semantic.onAccent} />}
        >
          {pending
            ? t("mobile:common.working")
            : stage === "enter-email"
              ? t("mobile:self.changeEmail.sendCode")
              : t("mobile:self.changeEmail.confirm")}
        </Button>
      </BottomAction>
    </Screen>
  );
}

export default observer(ChangeEmail);
