import React from "react";
import { useTranslation } from "react-i18next";
import type { ManifestChange } from "../../types";
import { changeKey, orderChanges, requiredWidenings } from "../../utils/botApps";
import { Switch } from "../ui/Switch";
import { changeText } from "./appText";

interface AppChangeListProps {
  changes: ReadonlyArray<ManifestChange>;
  /** Keys of the widenings the admin turned on. Empty to begin with: nothing is pre-accepted. */
  accepted: ReadonlySet<string>;
  onToggle: (key: string, on: boolean) => void;
  disabled?: boolean;
}

/**
 * An update's diff (#2005). Widenings come first, each on a solid warning edge with a "New
 * access" label and its own switch, off until the admin turns it on; the rest follow plainly.
 */
export const AppChangeList: React.FC<AppChangeListProps> = ({ changes, accepted, onToggle, disabled }) => {
  const { t } = useTranslation("channels");
  const needed = new Set(requiredWidenings(changes).map(changeKey));
  return (
    <div className="flex flex-col gap-2" data-testid="bot-app-changes">
      <h3 className="text-heading text-fg">{t("bots.app.changes.heading")}</h3>
      <ul className="flex flex-col gap-2">
        {orderChanges(changes).map((c) => {
          const key = changeKey(c);
          const text = changeText(t, c);
          if (!c.widening) {
            return (
              <li key={key} data-testid={`bot-app-change-${key}`} data-widening="false" className="px-3 text-body text-dim">
                {text}
              </li>
            );
          }
          return (
            <li
              key={key}
              data-testid={`bot-app-change-${key}`}
              data-widening="true"
              className="flex flex-col gap-2 rounded-panel border border-warning bg-surface-raised px-3 py-2"
            >
              <div className="flex flex-wrap items-center gap-2">
                <span className="rounded-chip bg-surface-high px-1.5 text-meta font-medium text-warning">
                  {t("bots.app.changes.newAccess")}
                </span>
                {needed.has(key) && <span className="text-meta text-muted">{t("bots.app.changes.needed")}</span>}
              </div>
              <span className="text-body text-fg">{text}</span>
              <Switch
                id={`bot-app-accept-${key}`}
                data-testid={`bot-app-accept-${key}`}
                label={t("bots.app.changes.allow")}
                checked={accepted.has(key)}
                disabled={disabled}
                onChange={(on) => onToggle(key, on)}
              />
            </li>
          );
        })}
      </ul>
    </div>
  );
};
