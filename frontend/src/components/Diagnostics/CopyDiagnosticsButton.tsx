import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { writeClipboardText } from "../../bridge";
import * as api from "../../services/api";
import { Button } from "../ui/Button";

type CopyState = "idle" | "copied" | "failed";

interface CopyDiagnosticsButtonProps {
  size?: "xs" | "sm" | "md";
  variant?: "primary" | "secondary" | "ghost";
  className?: string;
}

/// Puts the on-device diagnostics log (#1305) on the clipboard, for the user to
/// paste to whoever is helping them. Nothing is sent anywhere by this button.
export const CopyDiagnosticsButton: React.FC<CopyDiagnosticsButtonProps> = ({
  size = "sm",
  variant = "secondary",
  className,
}) => {
  const { t } = useTranslation("settings");
  const [state, setState] = useState<CopyState>("idle");
  const [busy, setBusy] = useState(false);

  const copy = async () => {
    setBusy(true);
    try {
      const report = await api.getDiagnostics();
      const ok = await writeClipboardText(report);
      setState(ok ? "copied" : "failed");
    } catch {
      setState("failed");
    } finally {
      setBusy(false);
    }
  };

  const label =
    state === "copied"
      ? t("diagnostics.copied")
      : state === "failed"
        ? t("diagnostics.copyFailed")
        : t("diagnostics.copy");

  return (
    <Button
      data-testid="copy-diagnostics-button"
      data-copy-state={state}
      type="button"
      size={size}
      variant={variant}
      isLoading={busy}
      onClick={copy}
      className={className}
    >
      {label}
    </Button>
  );
};
