import { useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import { useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { Screen, Header, Body, Card, Field, Button, BottomAction } from "../../components/ui";
import { SettingsField, ErrorText } from "../../components/self/SettingsField";
import { Icon } from "../../components/icons";
import { semantic, type as ty, space } from "../../theme/tokens";
import { useDeleteAccount } from "../../hooks/queries";
import { appStore } from "../../stores/appStore";

// The word the user must type to arm deletion. Matches desktop's
// SecurityPage: deliberately a constant, not translatable copy, so the
// comparison and the instruction can never drift apart.
const DELETE_CONFIRM_WORD = "DELETE";

function DeleteAccount() {
  const { t } = useTranslation("settings");
  const router = useRouter();
  const queryClient = useQueryClient();
  const currentUser = appStore.currentUser;
  const [confirmText, setConfirmText] = useState("");
  const deleteAccount = useDeleteAccount();

  const armed = confirmText === DELETE_CONFIRM_WORD;

  const onDelete = () => {
    if (!currentUser || !armed || deleteAccount.isPending) {
      return;
    }
    deleteAccount.mutate(currentUser.id, {
      onSuccess: () => {
        // The account is gone server-side and this device's data is wiped.
        // Drop everything the UI still holds (decrypted messages live in the
        // query cache) and land on the sign-in screen.
        queryClient.clear();
        appStore.logout();
        router.replace("/(auth)/email");
      },
    });
  };

  const confirmLabel = t("security.deleteConfirmLabel", { word: DELETE_CONFIRM_WORD });

  return (
    <Screen testID="screen-self-delete-account" centered>
      <Header title={t("mobile:self.deleteAccount.title")} backTo={t("security.title")} />
      <Body contentContainerStyle={{ paddingHorizontal: space.xxl, paddingTop: space.xxl, gap: space.xxl }}>
        <Card style={{ gap: space.lg }}>
          <View style={{ flexDirection: "row", alignItems: "center", gap: space.sm }}>
            <Icon.alert size={20} color={semantic.accent} />
            <Text accessibilityRole="header" style={[ty.heading, { flex: 1 }]}>
              {t("mobile:self.deleteAccount.irreversible")}
            </Text>
          </View>
          <Text style={[ty.body, { color: semantic.dim }]}>
            {t("mobile:self.deleteAccount.consequences")}
          </Text>
        </Card>

        <SettingsField label={confirmLabel}>
          <Field
            value={confirmText}
            onChangeText={setConfirmText}
            placeholder={DELETE_CONFIRM_WORD}
            autoCapitalize="characters"
            autoCorrect={false}
            testID="input-delete-confirm"
            accessibilityLabel={confirmLabel}
          />
        </SettingsField>

        {deleteAccount.isError ? (
          <ErrorText testID="text-delete-error">
            {(deleteAccount.error as Error).message || t("mobile:self.deleteAccount.failed")}
          </ErrorText>
        ) : null}
      </Body>
      <BottomAction>
        <Button
          full
          testID="btn-delete-account"
          icon={<Icon.trash size={20} color={semantic.text} />}
          disabled={!armed || deleteAccount.isPending || !currentUser}
          onPress={onDelete}
        >
          {deleteAccount.isPending
            ? t("security.deletingAccount")
            : t("mobile:self.deleteAccount.submit")}
        </Button>
      </BottomAction>
    </Screen>
  );
}

export default observer(DeleteAccount);
