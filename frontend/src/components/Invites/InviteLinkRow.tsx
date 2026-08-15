import React from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";
import { useSkin } from "../../hooks/queries/usePreferences";
import { activeLocale } from "../../utils/format";
import type { InviteLinkSummary } from "../../hooks/queries/useGroups";

interface InviteLinkRowProps {
  link: InviteLinkSummary;
  onRevoke: (linkId: string) => void;
  isRevoking: boolean;
}

/**
 * One existing invite link.
 *
 * There is no copy button here, and that is structural rather than an
 * oversight: `InviteLinkSummary` carries no token because the server has no
 * token to give. Only [`CreatedInviteLinkCard`] can copy, and only once.
 */
export const InviteLinkRow: React.FC<InviteLinkRowProps> = ({
  link,
  onRevoke,
  isRevoking,
}) => {
  const { t } = useTranslation("channels");
  const skin = useSkin();

  const usesLabel =
    link.max_uses != null
      ? t("inviteLinks.rowUsesOfMax", { used: link.uses, count: link.max_uses })
      : t("inviteLinks.rowUses", { count: link.uses });

  // `is_live` is computed server-side so this badge cannot disagree with what
  // redemption will actually do.
  let status: string;
  if (link.revoked_at) {
    status = t("inviteLinks.statusRevoked");
  } else if (link.is_live) {
    status = t("inviteLinks.statusActive");
  } else {
    status = t("inviteLinks.statusExpired");
  }

  const detail = [
    usesLabel,
    link.expires_at
      ? t("inviteLinks.expiresOn", {
          date: new Date(link.expires_at).toLocaleDateString(activeLocale()),
        })
      : t("inviteLinks.noExpiry"),
    link.creator_username
      ? t("inviteLinks.createdBy", { name: link.creator_username })
      : null,
  ]
    .filter(Boolean)
    .join(" · ");

  const statusClass = link.is_live ? "text-accent" : "text-muted";

  if (skin === "refined") {
    return (
      <div
        data-testid="invite-link-row"
        className="flex items-center justify-between gap-3 rounded-lg border border-line bg-surface px-3 py-2.5"
      >
        <div className="min-w-0">
          <p className={`text-sm font-medium ${statusClass}`}>{status}</p>
          <p className="truncate text-xs text-dim">{detail}</p>
        </div>
        {link.is_live && (
          <Button
            onClick={() => onRevoke(link.id)}
            variant="danger"
            size="xs"
            isLoading={isRevoking}
            data-testid="revoke-invite-link"
          >
            {t("inviteLinks.revoke")}
          </Button>
        )}
      </div>
    );
  }

  return (
    <div
      data-testid="invite-link-row"
      className="flex items-center justify-between gap-3 border border-line bg-surface px-3 py-2"
    >
      <div className="min-w-0">
        <p className={`text-sm ${statusClass}`}>[{status.toUpperCase()}]</p>
        <p className="truncate text-xs text-dim">{detail}</p>
      </div>
      {link.is_live && (
        <Button
          onClick={() => onRevoke(link.id)}
          variant="danger"
          size="xs"
          isLoading={isRevoking}
          data-testid="revoke-invite-link"
        >
          {t("inviteLinks.revokeTerminal")}
        </Button>
      )}
    </div>
  );
};
