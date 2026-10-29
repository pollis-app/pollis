import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertCircle, Check, Copy } from "lucide-react";
import { Button } from "./Button";
import { writeClipboardText } from "../../bridge";

/** Same three states as the message copy-link button (#889). */
type CopyState = "idle" | "copied" | "failed";

interface CopyStateLabels {
  copy: string;
  copied: string;
  failed: string;
}

interface CodeSnippetProps {
  value: string;
  labels?: Partial<CopyStateLabels>;
  ariaLabels?: CopyStateLabels;
  "data-testid"?: string;
  copyTestId?: string;
}

const COPY_FEEDBACK_MS = 2000;

/** Read-only value with a copy button, via the clipboard bridge (`navigator.clipboard` is unreliable on WebKitGTK) (#898). */
export const CodeSnippet: React.FC<CodeSnippetProps> = ({
  value,
  labels,
  ariaLabels,
  "data-testid": testId,
  copyTestId,
}) => {
  const { t } = useTranslation("common");
  const [copyState, setCopyState] = useState<CopyState>("idle");

  useEffect(() => {
    if (copyState === "idle") {
      return;
    }
    const timer = window.setTimeout(() => setCopyState("idle"), COPY_FEEDBACK_MS);
    return () => window.clearTimeout(timer);
  }, [copyState]);

  const handleCopy = async () => {
    // Back to idle first so a repeat click visibly changes state.
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
