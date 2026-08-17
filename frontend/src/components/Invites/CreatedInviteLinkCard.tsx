import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertCircle, Check, Copy } from "lucide-react";
import { Button } from "../ui/Button";
import { writeClipboardText } from "../../bridge";
import { useSkin } from "../../hooks/queries/usePreferences";
import { activeLocale } from "../../utils/format";
import type { CreatedInviteLink } from "../../hooks/queries/useGroups";

interface CreatedInviteLinkCardProps {
  link: CreatedInviteLink;
}

/** Outcome of the last copy, in the same three states the message
 *  copy-link button uses (#889). */
type CopyState = "idle" | "copied" | "failed";

/** How long an outcome stays on the button before it returns to idle. */
const COPY_FEEDBACK_MS = 2000;

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
  const [copyState, setCopyState] = useState<CopyState>("idle");

  // Clear the outcome a couple of seconds after it appears, and never leave a
  // timer behind that would set state on an unmounted card.
  useEffect(() => {
    if (copyState === "idle") {
      return;
    }
    const timer = window.setTimeout(() => setCopyState("idle"), COPY_FEEDBACK_MS);
    return () => window.clearTimeout(timer);
  }, [copyState]);

  const handleCopy = async () => {
    // Back to idle first so a second click on an already-failed button is a
    // visible state change, and so the feedback timer restarts.
    setCopyState("idle");
    // Through the clipboard bridge rather than `navigator.clipboard`: the
    // latter is unreliable on WebKitGTK, the Linux webview this app ships on.
    // A link that is only ever shown once and was never actually copied is the
    // worst possible thing to report as a success, so the false return — and a
    // rejected write — surface as a failed state instead of a console line
    // nobody reads (#898).
    const copied = await writeClipboardText(link.url).catch(() => false);
    setCopyState(copied ? "copied" : "failed");
  };

  const copyLabel =
    copyState === "copied"
      ? t("inviteLinks.copiedLabel")
      : copyState === "failed"
        ? t("inviteLinks.copyFailedLabel")
        : t("inviteLinks.copyLabel");
  const copyVariant =
    copyState === "copied" ? "secondary" : copyState === "failed" ? "danger" : "primary";

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
            variant={copyVariant}
            size="sm"
            data-testid="copy-invite-link"
            data-copy-state={copyState}
            aria-label={copyLabel}
          >
            <span className="flex items-center gap-1.5">
              {copyState === "copied" ? (
                <Check size={14} />
              ) : copyState === "failed" ? (
                <AlertCircle size={14} />
              ) : (
                <Copy size={14} />
              )}
              {copyState === "copied"
                ? t("inviteLinks.copied")
                : copyState === "failed"
                  ? t("inviteLinks.copyFailed")
                  : t("inviteLinks.copy")}
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
          variant={copyVariant}
          size="sm"
          data-testid="copy-invite-link"
          data-copy-state={copyState}
          aria-label={copyLabel}
        >
          {copyState === "copied"
            ? t("inviteLinks.copiedTerminal")
            : copyState === "failed"
              ? t("inviteLinks.copyFailedTerminal")
              : t("inviteLinks.copyTerminal")}
        </Button>
      </div>
      <p className="mt-2 text-xs text-muted">{boundsLabel}</p>
    </div>
  );
};
