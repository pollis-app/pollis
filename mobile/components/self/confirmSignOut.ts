import { Alert } from "react-native";
import i18n from "../../i18n";

/**
 * The confirm step in front of signing out. Destructive actions are told
 * apart by label and a confirmation, never by a third colour.
 */
export function confirmSignOut(onConfirm: () => void): void {
  Alert.alert(
    i18n.t("mobile:self.hub.signOutConfirmTitle"),
    undefined,
    [
      { text: i18n.t("common:actions.cancel"), style: "cancel" },
      {
        text: i18n.t("auth:shell.signOutTitle"),
        style: "destructive",
        onPress: onConfirm,
      },
    ],
    { cancelable: true },
  );
}
