import { useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import { useQueryClient } from "@tanstack/react-query";
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
import { useDeleteAccount } from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

// The word the user must type to arm deletion. Matches desktop's
// SecurityPage: deliberately a constant, not translatable copy, so the
// comparison and the instruction can never drift apart.
const DELETE_CONFIRM_WORD = "DELETE";

function DeleteAccount() {
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

  return (
    <Screen testID="screen-self-delete-account" centered>
      <Crumb
        segs={[
          { label: "SELF" },
          { label: "Security" },
          { label: "Delete account", leaf: true },
        ]}
      />
      <Body>
        <View style={{ paddingHorizontal: 18, paddingTop: 14, gap: 14 }}>
          <Text style={[ty.h1, { color: semantic.danger }]}>
            Delete account
          </Text>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              lineHeight: 19,
              color: semantic.mute,
            }}
          >
            This permanently deletes your Pollis account. You are removed from
            every group and conversation, your devices are unregistered, and
            your encrypted data on this phone is wiped. Other members keep
            their own copies of past messages, but nothing sent after deletion
            can ever be read with your keys.
          </Text>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              lineHeight: 19,
              color: semantic.mute,
            }}
          >
            This cannot be undone. There is no grace period and no recovery.
          </Text>

          <View style={{ gap: 6, paddingTop: 8 }}>
            <Text style={ty.label}>
              TYPE {DELETE_CONFIRM_WORD} TO CONFIRM
            </Text>
            <Field
              value={confirmText}
              onChangeText={setConfirmText}
              placeholder={DELETE_CONFIRM_WORD}
              testID="input-delete-confirm"
              accessibilityLabel={`Type ${DELETE_CONFIRM_WORD} to confirm`}
              icon={<Icon.shield color={semantic.danger} />}
            />
          </View>

          {deleteAccount.isError ? (
            <Text
              testID="text-delete-error"
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                lineHeight: 17,
                color: semantic.danger,
              }}
            >
              {(deleteAccount.error as Error).message ||
                "Couldn't delete the account. Nothing was changed — try again."}
            </Text>
          ) : null}
        </View>
      </Body>
      <BottomAction>
        <Button
          full
          variant="danger"
          testID="btn-delete-account"
          icon={<Icon.exit color={semantic.danger} />}
          disabled={!armed || deleteAccount.isPending || !currentUser}
          onPress={onDelete}
        >
          {deleteAccount.isPending
            ? "DELETING ACCOUNT…"
            : "DELETE ACCOUNT PERMANENTLY"}
        </Button>
      </BottomAction>
      <Ctx cr="SELF · SECURITY" name="Delete account" />
    </Screen>
  );
}

export default observer(DeleteAccount);
