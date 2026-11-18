import React from "react";
import { useTranslation } from "react-i18next";
import type { BotAppView } from "../../types";
import { appNotice } from "../../utils/botApps";
import { reasonText } from "./appText";

/**
 * The admin notice (#2005): why the bot is not running, from the runtime's report or from an
 * approval that does not verify on this device. A solid block, no glow; nothing while it runs.
 */
export const AppNotice: React.FC<{ view: BotAppView }> = ({ view }) => {
  const { t } = useTranslation("channels");
  const notice = appNotice(view);
  if (!notice) {
    return null;
  }
  return (
    <div
      role="status"
      data-testid="bot-app-notice"
      data-reason={notice.code}
      className="flex flex-col gap-1 rounded-panel border border-warning bg-surface-raised px-4 py-3"
    >
      <p className="text-body font-medium text-fg">{t("bots.app.notice.title")}</p>
      {notice.waiting && (
        <p data-testid="bot-app-notice-waiting" className="text-secondary text-fg">
          {t("bots.app.notice.waiting")}
        </p>
      )}
      <p data-testid="bot-app-notice-reason" className="text-secondary text-dim">
        {reasonText(t, notice.code, notice.detail)}
      </p>
    </div>
  );
};
