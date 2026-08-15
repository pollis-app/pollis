import React from "react";
import { Button } from "../ui/Button";
import { useSkin } from "../../hooks/queries/usePreferences";
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
  const skin = useSkin();

  const usesLabel =
    link.max_uses != null ? `${link.uses}/${link.max_uses} uses` : `${link.uses} uses`;

  // `is_live` is computed server-side so this badge cannot disagree with what
  // redemption will actually do.
  let status: string;
  if (link.revoked_at) {
    status = "Revoked";
  } else if (link.is_live) {
    status = "Active";
  } else {
    status = "Expired";
  }

  const detail = [
    usesLabel,
    link.expires_at
      ? `expires ${new Date(link.expires_at).toLocaleDateString()}`
      : "no expiry",
    link.creator_username ? `by @${link.creator_username}` : null,
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
            Revoke
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
          [REVOKE]
        </Button>
      )}
    </div>
  );
};
