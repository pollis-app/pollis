import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertCircle, Check, Copy } from "lucide-react";
import { Button } from "./Button";
import { writeClipboardText } from "../../bridge";

/** Outcome of the last copy, in the same three states the message
 *  copy-link button uses (#889). */
type CopyState = "idle" | "copied" | "failed";

/** One string per copy state. */
interface CopyStateLabels {
  copy: string;
  copied: string;
  failed: string;
}

interface CodeSnippetProps {
  /** The exact text shown and copied. */
  value: string;
  /** Visible button text per state; defaults to the generic copy wording. */
  labels?: Partial<CopyStateLabels>;
  /** Accessible name per state, when the visible text alone is ambiguous. */
  ariaLabels?: CopyStateLabels;
  /** Test id on the `<code>` element. */
  "data-testid"?: string;
  /** Test id on the copy button. */
  copyTestId?: string;
}

/** How long an outcome stays on the button before it returns to idle. */
const COPY_FEEDBACK_MS = 2000;

/**
 * A read-only value with a copy button beside it — a shell command, a link.
 *
 * The value wraps instead of scrolling: a horizontal scrollbar under a single
 * line of text is noise, and a wrapped value is still selectable whole. The
 * copy goes through the clipboard bridge rather than `navigator.clipboard`,
 * which is unreliable on WebKitGTK (the Linux webview), and a failed write is
 * shown as a failure instead of a silent success (#898).
 */
export const CodeSnippet: React.FC<CodeSnippetProps> = ({
  value,
  labels,
  ariaLabels,
  "data-testid": testId,
  copyTestId,
}) => {
  const { t } = useTranslation("common");
  const [copyState, setCopyState] = useState<CopyState>("idle");

  // Clear the outcome a couple of seconds after it appears, and never leave a
  // timer behind that would set state on an unmounted snippet.
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
    const copied = await writeClipboardText(value).catch(() => false);
    setCopyState(copied ? "copied" : "failed");
  };

  const text: CopyStateLabels = {
    copy: labels?.copy ?? t("actions.copy"),
    copied: labels?.copied ?? t("actions.copied"),
    failed: labels?.failed ?? t("actions.copyFailed"),
  };
  const stateKey = copyState === "idle" ? "copy" : copyState;
  const variant =
    copyState === "copied" ? "secondary" : copyState === "failed" ? "danger" : "primary";

  return (
    <div className="flex items-center gap-3 rounded-md border border-line bg-bg px-3 py-2">
      <code
        dir="ltr"
        data-testid={testId}
        className="min-w-0 flex-1 break-all font-mono text-xs text-fg"
      >
        {value}
      </code>
      <Button
        onClick={handleCopy}
        variant={variant}
        size="sm"
        className="flex-shrink-0"
        data-testid={copyTestId}
        data-copy-state={copyState}
        aria-label={ariaLabels?.[stateKey]}
      >
        {copyState === "copied" ? (
          <Check size={14} />
        ) : copyState === "failed" ? (
          <AlertCircle size={14} />
        ) : (
          <Copy size={14} />
        )}
        {text[stateKey]}
      </Button>
    </div>
  );
};
