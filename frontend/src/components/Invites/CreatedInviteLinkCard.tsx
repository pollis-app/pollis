import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Copy } from "lucide-react";
import { Button } from "../ui/Button";
import { useSkin } from "../../hooks/queries/usePreferences";
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
  const skin = useSkin();
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(link.url);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch (err) {
      console.error("Failed to copy invite link:", err);
    }
  };

  const bounds: string[] = [];
  if (link.max_uses != null) {
    bounds.push(t("inviteLinks.maxUses", { count: link.max_uses }));
  }
  if (link.expires_at) {
    bounds.push(
      t("inviteLinks.expiresOn", { date: new Date(link.expires_at).toLocaleString() }),
    );
  }
  const boundsLabel =
    bounds.length > 0 ? bounds.join(" · ") : t("inviteLinks.unbounded");

  if (skin === "refined") {
    return (
      <div
        data-testid="created-invite-link"
        className="rounded-lg border border-line-strong bg-surface-raised p-4"
      >
        <p className="text-sm font-semibold text-fg">{t("inviteLinks.created")}</p>
        <p className="mt-1 text-xs text-dim">{t("inviteLinks.copyNotice")}</p>
        <div className="mt-3 flex items-center gap-2">
          <code
            dir="ltr"
            data-testid="created-invite-link-url"
            className="min-w-0 flex-1 overflow-x-auto whitespace-nowrap rounded border border-line bg-surface px-3 py-2 text-xs text-fg"
          >
            {link.url}
          </code>
          <Button
            onClick={handleCopy}
            variant={copied ? "secondary" : "primary"}
            size="sm"
            data-testid="copy-invite-link"
            aria-label={t("inviteLinks.copyLabel")}
          >
            <span className="flex items-center gap-1.5">
              {copied ? <Check size={14} /> : <Copy size={14} />}
              {copied ? t("inviteLinks.copied") : t("inviteLinks.copy")}
            </span>
          </Button>
        </div>
        <p className="mt-2 text-xs text-muted">{boundsLabel}</p>
      </div>
    );
  }

  return (
    <div
      data-testid="created-invite-link"
      className="border border-line-strong bg-surface-raised p-3"
    >
      <p className="text-sm text-accent">{t("inviteLinks.createdTerminal")}</p>
      <p className="mt-1 text-xs text-dim">{t("inviteLinks.copyNotice")}</p>
      <div className="mt-2 flex items-center gap-2">
        <code
          dir="ltr"
          data-testid="created-invite-link-url"
          className="min-w-0 flex-1 overflow-x-auto whitespace-nowrap border border-line bg-bg px-2 py-1.5 text-xs text-fg"
        >
          {link.url}
        </code>
        <Button
          onClick={handleCopy}
          variant={copied ? "secondary" : "primary"}
          size="sm"
          data-testid="copy-invite-link"
          aria-label={t("inviteLinks.copyLabel")}
        >
          {copied ? t("inviteLinks.copiedTerminal") : t("inviteLinks.copyTerminal")}
        </Button>
      </div>
      <p className="mt-2 text-xs text-muted">{boundsLabel}</p>
    </div>
  );
};
