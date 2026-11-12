import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { NavigableList } from "../components/ui/NavigableList";
import { TextInput } from "../components/ui/TextInput";
import { ossPackageId, useOssNotices } from "../hooks/useOssNotices";
import type { OssNoticePackage } from "../types/ossNotices";

/// Every third-party package this build ships, with its licence (#1832). The
/// list is generated from the lockfiles at build time; selecting a package opens
/// the licence text it ships.
export const OpenSourcePage: React.FC = () => {
  const { t } = useTranslation("settings");
  const navigate = useNavigate();
  const state = useOssNotices();
  const [filter, setFilter] = useState("");

  const packages = state.status === "ready" ? state.notices.packages : [];
  const visible = useMemo(() => {
    const q = filter.trim().toLowerCase();
    if (!q) {
      return packages;
    }
    return packages.filter((p) => p.name.toLowerCase().includes(q) || p.license.toLowerCase().includes(q));
  }, [packages, filter]);

  return (
    <PageShell title={t("openSource.title")} scrollable>
      <div data-testid="open-source-page" className="flex-1 flex flex-col overflow-hidden">
        <div className="flex flex-col gap-3 px-6 pt-6 pb-3 font-mono text-xs">
          {state.status === "ready" && (
            <>
              <p data-testid="open-source-intro" className="text-fg">
                {t("openSource.intro")}
              </p>
              <p data-testid="open-source-count" className="text-dim">
                {t("openSource.count", { count: packages.length })}
              </p>
            </>
          )}
          {state.status === "unavailable" && (
            <p data-testid="open-source-unavailable" className="text-dim">
              {t("openSource.unavailable")}
            </p>
          )}
          <TextInput
            ariaLabel={t("openSource.filterLabel")}
            placeholder={t("openSource.filterPlaceholder")}
            value={filter}
            onChange={setFilter}
            disabled={state.status !== "ready"}
            data-testid="open-source-filter"
          />
        </div>
        <NavigableList<OssNoticePackage>
          testId="open-source-list"
          items={visible}
          getKey={ossPackageId}
          rowTestId={(p) => `open-source-row-${p.name}`}
          isLoading={state.status === "loading"}
          emptyLabel={t("openSource.empty")}
          autoFocus={false}
          activateRowOn="press"
          onEnterRow={(p) => void navigate({ to: "/open-source/$packageId", params: { packageId: ossPackageId(p) } })}
          onClickRow={(p) => void navigate({ to: "/open-source/$packageId", params: { packageId: ossPackageId(p) } })}
          renderRow={(p) => (
            <span className="flex flex-1 min-w-0 items-baseline gap-2">
              <span className="truncate text-fg">{p.name}</span>
              <span className="shrink-0 text-muted">{p.version}</span>
            </span>
          )}
          trailing={(p) => <span className="truncate text-dim">{p.license}</span>}
        />
      </div>
    </PageShell>
  );
};
