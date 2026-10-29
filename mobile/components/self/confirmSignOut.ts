import { Alert } from "react-native";
import i18n from "../../i18n";

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
