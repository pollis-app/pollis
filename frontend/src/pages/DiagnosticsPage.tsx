import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { PageShell } from "../components/Layout/PageShell";
import { CopyDiagnosticsButton } from "../components/Diagnostics/CopyDiagnosticsButton";
import * as api from "../services/api";
import { errorMessage } from "../utils/errorMessage";

/// The on-device diagnostics log (#1305), shown in full exactly as "Copy
/// diagnostics" would copy it.
export const DiagnosticsPage: React.FC = () => {
  const { t } = useTranslation("settings");
  const [report, setReport] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .getDiagnostics()
      .then((text) => {
        if (!cancelled) {
          setReport(text);
        }
      })
      .catch((err) => {
        if (!cancelled) {
          setLoadError(errorMessage(err, t("diagnostics.loadFailed")));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [t]);

  return (
    <PageShell title={t("diagnostics.title")} scrollable>
      <div data-testid="diagnostics-page" className="flex justify-center px-6 py-10">
        <div className="w-full max-w-2xl flex flex-col gap-4 font-mono text-xs">
          <p className="text-fg">{t("diagnostics.intro")}</p>
          <p className="text-dim">{t("diagnostics.privacy")}</p>
          <div>
            <CopyDiagnosticsButton size="md" variant="primary" />
          </div>
          {loadError && (
            <p data-testid="diagnostics-error" className="text-danger">
              {loadError}
            </p>
          )}
          <pre
            data-testid="diagnostics-preview"
            className="max-h-96 overflow-auto whitespace-pre-wrap break-words rounded-control border border-line bg-surface p-3 text-dim"
          >
            {report ?? t("common:states.loading")}
          </pre>
        </div>
      </div>
    </PageShell>
  );
};
