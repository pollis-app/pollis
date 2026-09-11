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
  const [fetching, setFetching] = useState(false);
  const [fetched, setFetched] = useState<api.FetchSummary | null>(null);
  const [fetchError, setFetchError] = useState<string | null>(null);

  const run = async () => {
    setError(null);
    setSummary(null);
    setFetched(null);
    setFetchError(null);
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

  // Opt-in and separately worded: the archive is already on disk by the time
  // this can be pressed, and it is the only path in the feature that talks to
  // the server.
  const missing = summary?.attachments_missing ?? [];
  const runFetch = async () => {
    if (!summary || missing.length === 0) {
      return;
    }
    setFetchError(null);
    setFetching(true);
    try {
      setFetched(await api.fetchExportAttachments(summary.files_dir, missing));
    } catch (err) {
      setFetchError(errorMessage(err, t("security.exportFetchError")));
    } finally {
      setFetching(false);
    }
  };
  const fetchStatus = fetched ? (
    <span data-testid={`${testId}-fetched`} className="text-xs font-mono text-muted">
      {[
        t("security.exportFetched", { count: fetched.fetched }),
        fetched.failed.length > 0 ? t("security.exportFetchFailed", { count: fetched.failed.length }) : null,
      ]
        .filter(Boolean)
        .join(" · ")}
    </span>
  ) : fetchError ? (
    <span data-testid={`${testId}-fetch-error`} className="text-xs font-mono text-danger">
      {fetchError}
    </span>
  ) : null;
  const fetchOffer =
    summary && missing.length > 0 && !fetched ? (
      <div className="flex flex-col gap-2">
        <p className="text-xs font-mono text-dim">{t("security.exportFetchNote")}</p>
        <Button
          data-testid={`${testId}-fetch`}
          onClick={runFetch}
          disabled={fetching}
          isLoading={fetching}
          loadingText={t("security.exportFetching")}
          variant="secondary"
          className="w-full"
        >
          {t("security.exportFetchButton", { count: missing.length })}
        </Button>
        {fetchStatus}
      </div>
    ) : (
      fetchStatus
    );

  const compact = variant === "icon";
  const done = summary
    ? t("security.exportDone", {
        messages: t("security.exportMessages", { count: summary.messages }),
        conversations: t("security.exportConversations", { count: summary.conversations }),
        size: formatFileSize(summary.bytes),
        path: summary.path,
      })
    : null;
  // Only distinct attachments are counted here, so "copied" + "missing" is
  // the number of files, not the number of references.
  const files =
    summary && summary.attachments > 0
      ? [
          t("security.exportAttachmentsWritten", { count: summary.attachments_written }),
          t("security.exportAttachmentsMissing", { count: summary.attachments_missing.length }),
        ].join(" · ")
      : null;
  const status = done ? (
    <span
      data-testid={`${testId}-done`}
      title={compact ? [done, files].filter(Boolean).join("\n") : undefined}
      className={`flex flex-col text-xs font-mono text-muted ${compact ? "truncate max-w-64" : "break-all"}`}
    >
      <span className={compact ? "truncate" : undefined}>{done}</span>
      {files && <span className={compact ? "truncate" : undefined}>{files}</span>}
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
      {fetchOffer}
    </div>
  );
};
