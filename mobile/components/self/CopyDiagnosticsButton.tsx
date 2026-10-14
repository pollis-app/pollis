import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui";
import { Icon } from "../icons";
import { semantic } from "../../theme/tokens";
import { copyDiagnostics } from "../../lib/diagnostics";

type CopyState = "idle" | "copied" | "failed";

/**
 * Puts the on-device diagnostics log (#1305) on the clipboard. Nothing is
 * sent anywhere by this button; the label says how the copy went.
 */
export function CopyDiagnosticsButton({
  variant = "secondary",
  full,
}: {
  variant?: "primary" | "secondary" | "subtle";
  full?: boolean;
}) {
  const { t } = useTranslation("settings");
  const [state, setState] = useState<CopyState>("idle");
  const [busy, setBusy] = useState(false);

  const onPress = async () => {
    setBusy(true);
    const ok = await copyDiagnostics();
    setState(ok ? "copied" : "failed");
    setBusy(false);
  };

  const label =
    state === "copied"
      ? t("diagnostics.copied")
      : state === "failed"
        ? t("diagnostics.copyFailed")
        : t("diagnostics.copy");

  return (
    <Button
      testID="btn-copy-diagnostics"
      variant={variant}
      full={full}
      disabled={busy}
      onPress={() => void onPress()}
      icon={<Icon.copy size={16} color={variant === "primary" ? semantic.onAccent : semantic.text} />}
    >
      {label}
    </Button>
  );
}
