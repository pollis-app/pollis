import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { Download } from "lucide-react";
import { Button } from "../ui/Button";
import { dialogSave } from "../../bridge";
import * as api from "../../services/api";
import { errorMessage } from "../../utils/errorMessage";
import { formatFileSize } from "../../utils/format";

interface Props {
  /// Narrows the export to one conversation; omit for the whole account.
  conversationId?: string;
  /// Suggested file name in the OS save dialog, without the `.json`.
  fileStem: string;
  /// `button` is the full-width settings-page control; `icon` is the compact
  /// header trigger with its status line rendered beside it.
  variant?: "button" | "icon";
  testId?: string;
}

/// One entry point for #856: pick a file, write the archive, report the
/// outcome inline. Strictly on-device — the write happens in Rust from the
/// local database; nothing here (or behind it) touches the network.
export const ExportArchiveButton: React.FC<Props> = ({
  conversationId,
  fileStem,
  variant = "button",
  testId = "export-archive",
}) => {
  const { t } = useTranslation("settings");
  const [busy, setBusy] = useState(false);
  const [summary, setSummary] = useState<api.ExportSummary | null>(null);
  const [error, setError] = useState<string | null>(null);

  const run = async () => {
    setError(null);
    setSummary(null);
    const target = await dialogSave({
      defaultPath: `${fileStem}.json`,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (!target) {
      return;
    }
    setBusy(true);
    try {
      setSummary(await api.exportArchive(target, conversationId));
    } catch (err) {
      setError(errorMessage(err, t("security.exportFailed")));
    } finally {
      setBusy(false);
    }
  };

  const compact = variant === "icon";
  const done = summary
    ? t("security.exportDone", {
        messages: t("security.exportMessages", { count: summary.messages }),
        conversations: t("security.exportConversations", { count: summary.conversations }),
        size: formatFileSize(summary.bytes),
        path: summary.path,
      })
    : null;
  const status = done ? (
    <span
      data-testid={`${testId}-done`}
      title={compact ? done : undefined}
      className={`text-xs font-mono text-muted ${compact ? "truncate max-w-64" : "break-all"}`}
    >
      {done}
    </span>
  ) : error ? (
    <span
      data-testid={`${testId}-error`}
      title={compact ? error : undefined}
      className={`text-xs font-mono text-danger ${compact ? "truncate max-w-64" : ""}`}
    >
      {error}
    </span>
  ) : null;

  if (compact) {
    return (
      <div className="flex items-center gap-2 min-w-0">
        {status}
        <button
          data-testid={testId}
          onClick={run}
          disabled={busy}
          aria-label={t("security.exportConversationLabel")}
          title={t("security.exportConversationLabel")}
          className="icon-btn-sm flex-shrink-0 padding-0"
        >
          <Download size={14} aria-hidden="true" />
        </button>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      <Button
        data-testid={testId}
        onClick={run}
        disabled={busy}
        isLoading={busy}
        loadingText={t("security.exporting")}
        variant="secondary"
        className="w-full"
      >
        {conversationId ? t("security.exportConversationButton") : t("security.exportButton")}
      </Button>
      {status}
    </div>
  );
};
