import React from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";
import { CreatedInviteLinkCard } from "../Invites/CreatedInviteLinkCard";
import type { DiscordImportResult } from "../../types/import";

interface ImportResultViewProps {
  result: DiscordImportResult;
  onOpenGroup: () => void;
}

/** What an import did: counts, anything that failed (a re-run retries it), and the invite link to share. */
export const ImportResultView: React.FC<ImportResultViewProps> = ({ result: r, onOpenGroup }) => {
  const { t } = useTranslation("import");
  const existing = r.categories_existing + r.channels_existing + r.emoji_existing;
  return (
    <div className="flex flex-col gap-5" data-testid="import-result">
      <div className="flex flex-col gap-2">
        <h3 className="text-heading font-semibold text-fg" data-testid="import-result-title">
          {t("discord.doneTitle", { name: r.group_name })}
        </h3>
        <p className="text-body text-dim" data-testid="import-result-counts">
          {t("discord.doneCounts", {
            categories: r.categories_created,
            channels: r.channels_created,
            emoji: r.emoji_uploaded,
          })}
        </p>
        {existing > 0 && <p className="text-secondary text-muted">{t("discord.doneExisting", { count: existing })}</p>}
      </div>

      {r.failures.length > 0 && (
        <div className="flex flex-col gap-1" data-testid="import-result-failures">
          <p className="text-secondary text-danger">{t("discord.doneFailures")}</p>
          <ul className="flex list-disc flex-col gap-1 ps-5 text-secondary text-muted">
            {r.failures.map((f, i) => (
              <li key={i}>{t("discord.failure", { name: f.name, error: f.error })}</li>
            ))}
          </ul>
        </div>
      )}

      {r.invite_link && <CreatedInviteLinkCard link={r.invite_link} />}
      {!r.invite_link && r.invite_link_existing && (
        <p className="text-secondary text-muted" data-testid="import-result-link-existing">
          {t("discord.linkExisting")}
        </p>
      )}

      <div>
        <Button onClick={onOpenGroup} data-testid="import-open-group">
          {t("discord.openGroup")}
        </Button>
      </div>
    </div>
  );
};
