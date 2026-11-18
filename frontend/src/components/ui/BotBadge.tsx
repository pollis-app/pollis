import React from "react";
import { useTranslation } from "react-i18next";
import { useSkin } from "../../hooks/queries/usePreferences";

interface BotBadgeProps {
  /** The bot's group; names who manages it in the tooltip. Omitted, the tooltip says "Bot". */
  groupName?: string;
  "data-testid"?: string;
}

/**
 * The Bot badge (#1182): marks a bot account beside its name in the member list, on its
 * messages and in mentions. A quiet tag on `surfaceHigh` in `textDim`, never the accent; the
 * chip radius. Terminal reads `[bot]`. Its tooltip says who manages the bot ("Managed by the
 * admins of Acme"); the bot's profile says it in words too, so nothing hides behind a hover.
 */
export const BotBadge: React.FC<BotBadgeProps> = ({ groupName, "data-testid": testId = "bot-badge" }) => {
  const { t } = useTranslation("channels");
  const isTerminal = useSkin() === "terminal";
  const managedBy = groupName ? t("bots.badge.managedBy", { group: groupName }) : undefined;
  return (
    <span
      data-testid={testId}
      title={managedBy ?? t("bots.badge.label")}
      className={
        isTerminal
          ? "flex-shrink-0 self-center font-mono text-meta text-dim select-none"
          : "flex-shrink-0 self-center rounded-chip bg-surface-high px-1.5 text-meta font-medium text-dim select-none"
      }
    >
      {isTerminal ? t("bots.badge.labelTerminal") : t("bots.badge.label")}
    </span>
  );
};
