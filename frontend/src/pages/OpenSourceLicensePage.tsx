import React from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { ossPackageId, useOssNotices } from "../hooks/useOssNotices";

/// One package's licence notice: the licence files it ships, or the standard
/// SPDX text when it ships none (#1832).
export const OpenSourceLicensePage: React.FC = () => {
  const { t } = useTranslation("settings");
  const { packageId } = useParams({ from: "/open-source/$packageId" });
  const state = useOssNotices();
  const pkg = state.status === "ready" ? state.notices.packages.find((p) => ossPackageId(p) === packageId) : undefined;

  return (
    <PageShell title={pkg ? pkg.name : t("openSource.title")} scrollable>
      <div data-testid="open-source-license-page" className="flex justify-center px-6 py-10">
        <div className="w-full max-w-2xl flex flex-col gap-4 font-mono text-xs">
          {state.status === "loading" && <p className="text-dim">{t("common:states.loading")}</p>}
          {state.status !== "loading" && !pkg && (
            <p data-testid="open-source-not-found" className="text-dim">
              {t("openSource.notFound")}
            </p>
          )}
          {pkg && state.status === "ready" && (
            <>
              <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
                <dt className="text-muted">{t("openSource.versionLabel")}</dt>
                <dd data-testid="open-source-version" className="text-fg">{pkg.version}</dd>
                <dt className="text-muted">{t("openSource.licenseLabel")}</dt>
                <dd data-testid="open-source-license" className="text-fg">{pkg.license}</dd>
                <dt className="text-muted">{t("openSource.sourceLabel")}</dt>
                <dd className="text-fg">
                  {pkg.ecosystem === "cargo" ? t("openSource.cargo") : t("openSource.npm")}
                </dd>
              </dl>
              {pkg.texts.map((i) => (
                <pre
                  key={i}
                  data-testid="open-source-text"
                  className="overflow-auto whitespace-pre-wrap break-words rounded-control border border-line bg-surface p-3 text-dim"
                >
                  {state.notices.texts[i]}
                </pre>
              ))}
            </>
          )}
        </div>
      </div>
    </PageShell>
  );
};
