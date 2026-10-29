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
 * #847 — the only view of a freshly minted invite link. The server stores only
 * `sha256(secret)`, so the link can never be shown again; the card says so.
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
      {/* A failed copy of a once-shown link must read as failed (#898). */}
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
