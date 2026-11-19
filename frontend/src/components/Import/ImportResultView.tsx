import React from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";
import { CreatedInviteLinkCard } from "../Invites/CreatedInviteLinkCard";
import type { EmailInviteOutcome, ImportResult } from "../../types/import";

interface ImportResultViewProps {
  result: ImportResult;
  /** The source had categories (Discord); Slack has none, so its counts leave them out. */
  withCategories: boolean;
  onOpenGroup: () => void;
}

/** What an import did: counts, invites sent, anything that failed (a re-run retries it), and the invite link to share. */
export const ImportResultView: React.FC<ImportResultViewProps> = ({ result: r, withCategories, onOpenGroup }) => {
  const { t } = useTranslation("import");
  const existing = r.categories_existing + r.channels_existing + r.emoji_existing;
  const counts = { categories: r.categories_created, channels: r.channels_created, emoji: r.emoji_uploaded };
  const by = (o: EmailInviteOutcome) => r.email_invites.filter((i) => i.outcome === o);
  const noAccount = by("no_account");
  const inviteLines = [
    { key: "invited", count: by("invited").length, text: t("result.invited", { count: by("invited").length }) },
    {
      key: "already_invited",
      count: by("already_invited").length,
      text: t("result.alreadyInvited", { count: by("already_invited").length }),
    },
    {
      key: "already_member",
      count: by("already_member").length,
      text: t("result.alreadyMember", { count: by("already_member").length }),
    },
    {
      key: "no_account",
      count: noAccount.length,
      text: t("result.noAccount", {
        count: noAccount.length,
        names: noAccount.map((i) => i.display_name).join(", "),
      }),
    },
  ].filter((l) => l.count > 0);

  return (
    <div className="flex flex-col gap-5" data-testid="import-result">
      <div className="flex flex-col gap-2">
        <h3 className="text-heading font-semibold text-fg" data-testid="import-result-title">
          {t("result.doneTitle", { name: r.group_name })}
        </h3>
        <p className="text-body text-dim" data-testid="import-result-counts">
          {withCategories ? t("result.doneCounts", counts) : t("result.doneCountsNoCategories", counts)}
        </p>
        {existing > 0 && <p className="text-secondary text-muted">{t("result.doneExisting", { count: existing })}</p>}
      </div>

      {inviteLines.length > 0 && (
        <div className="flex flex-col gap-1" data-testid="import-result-invites">
          <h3 className="text-heading font-semibold text-fg">{t("result.invitesTitle")}</h3>
          {inviteLines.map((l) => (
            <p key={l.key} className="text-secondary text-dim" data-testid={`import-result-invites-${l.key}`}>
              {l.text}
            </p>
          ))}
        </div>
      )}

      {r.failures.length > 0 && (
        <div className="flex flex-col gap-1" data-testid="import-result-failures">
          <p className="text-secondary text-danger">{t("result.doneFailures")}</p>
          <ul className="flex list-disc flex-col gap-1 ps-5 text-secondary text-muted">
            {r.failures.map((f, i) => (
              <li key={i}>{t("result.failure", { name: f.name, error: f.error })}</li>
            ))}
          </ul>
        </div>
      )}

      {r.invite_link && <CreatedInviteLinkCard link={r.invite_link} />}
      {!r.invite_link && r.invite_link_existing && (
        <p className="text-secondary text-muted" data-testid="import-result-link-existing">
          {t("result.linkExisting")}
        </p>
      )}

      <div>
        <Button onClick={onOpenGroup} data-testid="import-open-group">
          {t("result.openGroup")}
        </Button>
      </div>
    </div>
  );
};
