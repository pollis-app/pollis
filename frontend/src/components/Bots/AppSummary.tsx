import React from "react";
import { useTranslation } from "react-i18next";
import type { AppManifest } from "../../types";
import { CodeSnippet } from "../ui/CodeSnippet";
import { LinkButton } from "../ui/LinkButton";
import { shellOpen } from "../../bridge";
import { isLinkableSource, keyFingerprint } from "../../utils/botApps";

interface AppSummaryProps {
  manifest: AppManifest;
  /** Production: the App hash, copyable. */
  appHash: string | null;
  /** Development: shown as a short fingerprint. */
  developerKey: string | null;
  source: string | null;
  /** Marks beside the name ("Development build", …). */
  marks?: React.ReactNode;
  /** Who approved it and when, under the facts. */
  footer?: React.ReactNode;
  testId: string;
}

/** An App's identity (#2005): name, version, description, what pins it, and where it came from. */
export const AppSummary: React.FC<AppSummaryProps> = ({
  manifest,
  appHash,
  developerKey,
  source,
  marks,
  footer,
  testId,
}) => {
  const { t } = useTranslation("channels");
  return (
    <div className="flex flex-col gap-3" data-testid={testId}>
      <div className="flex flex-col gap-1">
        <div className="flex flex-wrap items-center gap-2">
          <bdi data-testid={`${testId}-name`} className="text-heading text-fg">
            {manifest.name}
          </bdi>
          <span data-testid={`${testId}-version`} className="text-secondary text-muted">
            {t("bots.app.version", { version: manifest.version })}
          </span>
          {marks}
        </div>
        {manifest.description && <p className="text-body text-dim">{manifest.description}</p>}
      </div>
      {developerKey ? (
        <div className="flex flex-col gap-1">
          <span className="text-secondary text-muted">{t("bots.app.developerKey")}</span>
          <code dir="ltr" data-testid={`${testId}-developer-key`} className="font-machine text-secondary text-fg">
            {keyFingerprint(developerKey)}
          </code>
        </div>
      ) : (
        appHash && (
          <div className="flex flex-col gap-1">
            <span className="text-secondary text-muted">{t("bots.app.appHash")}</span>
            <CodeSnippet value={appHash} data-testid={`${testId}-hash`} copyTestId={`${testId}-hash-copy`} />
          </div>
        )
      )}
      {source && (
        <div className="flex flex-col gap-1">
          <span className="text-secondary text-muted">{t("bots.app.source")}</span>
          {isLinkableSource(source) ? (
            <span className="break-all text-secondary" dir="ltr">
              <LinkButton data-testid={`${testId}-source`} onClick={() => void shellOpen(source)}>
                {source}
              </LinkButton>
            </span>
          ) : (
            <code dir="ltr" data-testid={`${testId}-source`} className="break-all font-machine text-secondary text-fg">
              {source}
            </code>
          )}
        </div>
      )}
      {footer}
    </div>
  );
};
