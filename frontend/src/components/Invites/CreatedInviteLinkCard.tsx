import React from "react";
import { useTranslation } from "react-i18next";
import { Card } from "../ui/Card";
import { CodeSnippet } from "../ui/CodeSnippet";
import { activeLocale } from "../../utils/format";
import type { CreatedInviteLink } from "../../hooks/queries/useGroups";

interface CreatedInviteLinkCardProps {
  link: CreatedInviteLink;
}

/**
 * #847 — the one and only view of a freshly minted invite link.
 *
 * This card is emphatic about being the last chance to copy on purpose. The
 * server stores only `sha256(secret)`, so unlike Slack or Discord — which keep
 * the code in plaintext and let you re-open it forever — this link genuinely
 * cannot be shown again by anyone, including us. That is the visible cost of
 * "a database read must not yield working invites", and hiding it would just
 * turn into a support question later.
 */
export const CreatedInviteLinkCard: React.FC<CreatedInviteLinkCardProps> = ({ link }) => {
  const { t } = useTranslation("channels");

  const bounds: string[] = [];
  if (link.max_uses != null) {
    bounds.push(t("inviteLinks.maxUses", { count: link.max_uses }));
  }
  if (link.expires_at) {
    bounds.push(
      t("inviteLinks.expiresOn", {
        date: new Date(link.expires_at).toLocaleString(activeLocale()),
      }),
    );
  }
  const boundsLabel =
    bounds.length > 0 ? bounds.join(" · ") : t("inviteLinks.unbounded");

  return (
    <Card padding="sm" data-testid="created-invite-link" className="flex flex-col gap-3">
      <div>
        <p className="text-sm font-semibold text-fg">{t("inviteLinks.created")}</p>
        <p className="mt-1 text-xs text-dim">{t("inviteLinks.copyNotice")}</p>
      </div>
      {/* A link that is only ever shown once and was never actually copied is
          the worst thing to report as a success, so the snippet surfaces a
          failed write as a failed state (#898). */}
      <CodeSnippet
        value={link.url}
        data-testid="created-invite-link-url"
        copyTestId="copy-invite-link"
        labels={{
          copy: t("inviteLinks.copy"),
          copied: t("inviteLinks.copied"),
          failed: t("inviteLinks.copyFailed"),
        }}
        ariaLabels={{
          copy: t("inviteLinks.copyLabel"),
          copied: t("inviteLinks.copiedLabel"),
          failed: t("inviteLinks.copyFailedLabel"),
        }}
      />
      <p className="text-xs text-muted">{boundsLabel}</p>
    </Card>
  );
};
