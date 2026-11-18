import React from "react";
import { useTranslation } from "react-i18next";
import type { AdminGrants, AppManifest } from "../../types";
import { appLines } from "../../utils/botApps";
import { appLineText } from "./appText";

interface AppCapabilitiesProps {
  manifest: AppManifest;
  grants: AdminGrants;
  testId: string;
}

/** "What it can do" (#2005): the App's effective scope (manifest ∩ grants), one plain line each. */
export const AppCapabilities: React.FC<AppCapabilitiesProps> = ({ manifest, grants, testId }) => {
  const { t } = useTranslation("channels");
  const lines = appLines(manifest, grants);
  return (
    <div className="flex flex-col gap-2" data-testid={testId}>
      <h3 className="text-heading text-fg">{t("bots.app.canDoHeading")}</h3>
      <ul className="flex flex-col gap-1">
        {lines.map((line) => (
          <li key={line.kind} data-testid={`${testId}-${line.kind}`} className="text-body text-dim">
            {appLineText(t, line)}
          </li>
        ))}
      </ul>
    </div>
  );
};
