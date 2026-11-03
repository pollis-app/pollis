import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { dialogSave } from "../../bridge";
import * as api from "../../services/api";
import { Button } from "../ui/Button";
import { diagnosticsFileName } from "../../utils/diagnosticsReport";

type SaveState = "idle" | "saved" | "failed";

/// Saves the whole on-device diagnostics log (#1305) as a .txt where the user picks; nothing is sent.
export const SaveDiagnosticsButton: React.FC = () => {
  const { t } = useTranslation("settings");
  const [state, setState] = useState<SaveState>("idle");
  const [busy, setBusy] = useState(false);

  const save = async () => {
    const target = await dialogSave({
      defaultPath: diagnosticsFileName(),
      filters: [{ name: "Text", extensions: ["txt"] }],
    });
    if (!target) {
      return;
    }
    setBusy(true);
    try {
      await api.saveDiagnostics(target);
      setState("saved");
    } catch {
      setState("failed");
    } finally {
      setBusy(false);
    }
  };

  const label =
    state === "saved"
      ? t("diagnostics.saved")
      : state === "failed"
        ? t("diagnostics.saveFailed")
        : t("diagnostics.save");

  return (
    <Button
      data-testid="save-diagnostics-button"
      type="button"
      size="md"
      variant="secondary"
      isLoading={busy}
      onClick={save}
    >
      {label}
    </Button>
  );
};
