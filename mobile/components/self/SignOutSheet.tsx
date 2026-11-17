import { useState } from "react";
import { useTranslation } from "react-i18next";
import { SheetOverlay, afterSheetClose } from "../chat/SheetOverlay";
import { Group, ListRow } from "../ui";
import { Icon } from "../icons";
import { useTheme } from "../theme";
import { semantic, fonts } from "../../theme/tokens";

/** Sign out asks first, in the app's own sheet (#1971): Close cancels, the one row signs out. */
export function SignOutSheet({
  onConfirm,
  onClose,
}: {
  onConfirm: () => void;
  onClose: () => void;
}) {
  useTheme();
  const { t } = useTranslation();
  return (
    <SheetOverlay
      title={t("mobile:self.hub.signOutConfirmTitle")}
      onClose={onClose}
      testID="sheet-sign-out"
    >
      {/* Signing out ends the session: on release, never touch-down. */}
      <Group surface="high">
        <ListRow
          testID="btn-confirm-sign-out"
          glyph={<Icon.logOut size={22} color={semantic.danger} />}
          name={t("auth:shell.signOutTitle")}
          nameStyle={{ fontFamily: fonts.semibold, color: semantic.danger }}
          onPress={onConfirm}
        />
      </Group>
    </SheetOverlay>
  );
}

/** The sheet's open state for a screen with a Sign out row; render `sheet` inside the screen. */
export function useSignOutSheet(onConfirm: () => void): {
  open: () => void;
  sheet: React.ReactNode;
} {
  const [shown, setShown] = useState(false);
  const sheet = shown ? (
    <SignOutSheet
      onClose={() => setShown(false)}
      onConfirm={() => {
        setShown(false);
        // Sign-out replaces the whole stack; iOS drops a navigation raced against the dismissal.
        afterSheetClose(onConfirm);
      }}
    />
  ) : null;
  return { open: () => setShown(true), sheet };
}
