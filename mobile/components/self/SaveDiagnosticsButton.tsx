import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui";
import { Icon } from "../icons";
import { semantic } from "../../theme/tokens";
import { saveDiagnostics } from "../../lib/diagnostics";

/** "Download": the whole diagnostics log (#1305) as a .txt through the share sheet; sends nothing itself. */
export function SaveDiagnosticsButton() {
  const { t } = useTranslation("settings");
  const [state, setState] = useState<"idle" | "saved" | "failed">("idle");
  const [busy, setBusy] = useState(false);

  const onPress = async () => {
    setBusy(true);
    setState((await saveDiagnostics(t("diagnostics.save"))) ? "saved" : "failed");
    setBusy(false);
  };

  return (
    <Button
      testID="btn-save-diagnostics"
      variant="secondary"
      full
      disabled={busy}
      onPress={() => void onPress()}
      icon={<Icon.download size={16} color={semantic.text} />}
    >
      {state === "saved" ? t("diagnostics.saved") : state === "failed" ? t("diagnostics.saveFailed") : t("diagnostics.save")}
    </Button>
  );
}
