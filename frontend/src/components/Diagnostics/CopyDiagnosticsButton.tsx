import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { writeClipboardText } from "../../bridge";
import * as api from "../../services/api";
import { Copy } from "lucide-react";
import { Button } from "../ui/Button";
import { recentReport } from "../../utils/diagnosticsReport";

type CopyState = "idle" | "copied" | "failed";

interface CopyDiagnosticsButtonProps {
  size?: "xs" | "sm" | "md";
  variant?: "primary" | "secondary" | "ghost";
  className?: string;
  /// "Copy" with an icon beside "Download" on the Diagnostics page; "Copy diagnostics" where it stands alone.
  short?: boolean;
}

/// Copies the newest part of the on-device diagnostics log (#1305); nothing is sent.
export const CopyDiagnosticsButton: React.FC<CopyDiagnosticsButtonProps> = ({
  size = "sm",
  variant = "secondary",
  className,
  short,
}) => {
  const { t } = useTranslation("settings");
  const [state, setState] = useState<CopyState>("idle");
  const [busy, setBusy] = useState(false);

  const copy = async () => {
    setBusy(true);
    try {
      const report = await api.getDiagnostics();
      const ok = await writeClipboardText(recentReport(report).text);
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
        : t(short ? "diagnostics.copyShort" : "diagnostics.copy");

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
      {short ? (
        <>
          <Copy size={14} aria-hidden="true" />
          {label}
        </>
      ) : (
        label
      )}
    </Button>
  );
};
