import React, { useMemo, useState } from "react";
import { Trans, useTranslation } from "react-i18next";
import i18n from "../../i18n";
import { TitleBar } from "../Layout/TitleBar";
import { DotMatrix } from "../ui/DotMatrix";
import { Card } from "../ui/Card";
import { Button } from "../ui/Button";
import { TextInput } from "../ui/TextInput";
import { logIgnored } from "../../utils/log";

interface SaveSecretKeyScreenProps {
  /// The freshly-generated Secret Key returned from `verify_otp`. Shown
  /// to the user once. We never store it on disk; once they confirm we
  /// pass control back to the parent.
  secretKey: string;
  /// Email address of the account this key belongs to — used as a
  /// suffix on the downloaded emergency-kit filename so users with
  /// multiple accounts can tell their kits apart. Optional; omitted →
  /// unsuffixed filename.
  email?: string | null;
  /// Called once the user has typed the key back to confirm they saved it.
  onConfirmed: () => void;
}

/// Normalize for comparison: strip whitespace, dashes, prefix, uppercase.
/// Mirrors the backend `normalize_secret_key` so partial / re-typed user
/// input round-trips against the original.
function normalize(input: string): string {
  return input
    .replace(/\s+/g, "")
    .replace(/^A3-?/i, "")
    .replace(/-/g, "")
    .toUpperCase();
}

// Restrict filename chars to a conservative alphanumeric/hyphen/
// underscore set so any exotic input (emoji, slashes, whitespace)
// can't produce an invalid filename on any OS.
function sanitizeForFilename(input: string): string {
  return input.replace(/[^a-zA-Z0-9_-]+/g, "_").replace(/^_+|_+$/g, "");
}

// Compact YYYYMMDD-HHMMSS in local time.
function compactTimestamp(d: Date): string {
  const p = (n: number) => n.toString().padStart(2, "0");
  return `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`;
}

function downloadEmergencyKit(secretKey: string, email?: string | null) {
  // One-shot file contents, produced on click and never re-rendered, so the
  // i18n singleton is the right lookup here rather than a `useTranslation` `t`.
  const text = i18n.t("auth:emergencyKit.document", {
    secretKey,
    generated: new Date().toISOString(),
  });

  const parts = ["pollis-emergency-kit"];
  if (import.meta.env.DEV) {
    parts.push("DEV");
  }
  const safeEmail = email ? sanitizeForFilename(email) : "";
  if (safeEmail) {
    parts.push(safeEmail);
  }
  parts.push(compactTimestamp(new Date()));
  const filename = `${parts.join("-")}.txt`;

  const blob = new Blob([text], { type: "text/plain" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

type Phase = "warn" | "show" | "confirm";

export const SaveSecretKeyScreen: React.FC<SaveSecretKeyScreenProps> = ({
  secretKey,
  email,
  onConfirmed,
}) => {
  const { t } = useTranslation("auth");
  const [phase, setPhase] = useState<Phase>("warn");
  const [confirmInput, setConfirmInput] = useState("");
  const [showError, setShowError] = useState(false);
  const [copied, setCopied] = useState(false);
  const [downloaded, setDownloaded] = useState(false);

  const normalizedTarget = useMemo(() => normalize(secretKey), [secretKey]);
  const normalizedInput = normalize(confirmInput);
  const matches = normalizedInput === normalizedTarget && normalizedTarget.length > 0;

  const handleConfirm = () => {
    if (!matches) {
      setShowError(true);
      return;
    }
    onConfirmed();
  };

  // Phase 1: warning screen
  if (phase === "warn") {
    return (
      <div
        data-testid="save-secret-key-warning-screen"
        className="flex flex-col h-full w-full bg-bg"
        style={{ position: "relative" }}
      >
        <div style={{ position: "absolute", inset: 0, opacity: 0.2, pointerEvents: "none" }}>
          <DotMatrix speed={0.2} />
        </div>
        <TitleBar />
        <div
          className="flex-1 flex justify-center overflow-y-auto"
          style={{ position: "relative", zIndex: 1, padding: "1rem" }}
        >
          <Card padding="lg" className="my-auto" style={{ width: "100%", maxWidth: 480 }}>
            <div className="flex flex-col gap-5">
              <div>
                <h1 className="text-base font-mono font-bold mb-8 text-danger">
                  {t("secretKey.warnTitle")}
                </h1>
                <p
                  className="text-xs mt-2 font-mono text-fg"
                  style={{ lineHeight: 1.6 }}
                >
                  <Trans
                    t={t}
                    i18nKey="secretKey.warnIntro"
                    components={{ strong: <strong /> }}
                  />
                </p>
                <p
                  className="text-xs mt-4 font-mono text-dim"
                  style={{ lineHeight: 1.6 }}
                >
                  <Trans
                    t={t}
                    i18nKey="secretKey.warnOnce"
                    components={{ emph: <strong className="text-fg" /> }}
                  />
                </p>
                <p
                  className="text-xs mt-3 mb-4 font-mono text-fg"
                  style={{ lineHeight: 1.6 }}
                >
                  {t("secretKey.warnPrepare")}
                </p>
              </div>
              <Button
                data-testid="save-secret-key-acknowledge-button"
                onClick={() => setPhase("show")}
                className="w-full"
              >
                {t("secretKey.acknowledge")}
              </Button>
            </div>
          </Card>
        </div>
      </div>
    );
  }

  // Phase 2: reveal key + copy/download actions
  if (phase === "show") {
    return (
      <div
        data-testid="save-secret-key-screen"
        className="flex flex-col h-full w-full bg-bg"
        style={{ position: "relative" }}
      >
        <div style={{ position: "absolute", inset: 0, opacity: 0.2, pointerEvents: "none" }}>
          <DotMatrix speed={0.2} />
        </div>
        <TitleBar />
        <div
          className="flex-1 flex justify-center overflow-y-auto"
          style={{ position: "relative", zIndex: 1, padding: "1rem", overflowY: "auto" }}
        >
          <Card padding="lg" className="my-auto" style={{ width: "100%", maxWidth: 480 }}>
            <div className="flex flex-col gap-5">
              <div>
                <h1 className="text-base font-mono font-bold mb-4 text-accent">
                  {t("secretKey.showTitle")}
                </h1>
                <p className="text-xs mt-1 font-mono text-dim">
                  {t("secretKey.showSubtitle")}
                </p>
              </div>

              <div
                data-testid="secret-key-display"
                className="font-mono text-sm select-all bg-surface border-2 border-accent text-accent"
                style={{
                  borderRadius: "0.5rem",
                  padding: "0.5rem",
                  wordBreak: "break-all",
                  textAlign: "center",
                  letterSpacing: "0.05em",
                  marginBottom: "1rem",
                }}
              >
                {secretKey}
              </div>

              <div className="flex flex-col gap-2 mb-4 items-center">

                <Button
                  data-testid="download-secret-key-button"
                  onClick={() => {
                    downloadEmergencyKit(secretKey, email);
                    setDownloaded(true);
                    window.setTimeout(() => setDownloaded(false), 2000);
                  }}
                  variant="primary"
                  className="w-full"
                  size="sm"
                >
                  {downloaded ? t("secretKey.downloaded") : t("secretKey.download")}
                </Button>
                <Button
                  data-testid="copy-secret-key-button"
                  onClick={() => {
                    navigator.clipboard
                      .writeText(secretKey)
                      .then(() => {
                        setCopied(true);
                        window.setTimeout(() => setCopied(false), 2000);
                      })
                      .catch(logIgnored);
                  }}
                  variant="ghost"
                  className="w-fit-content mx-auto"
                  size="sm"
                >
                  {copied ? t("secretKey.copied") : t("secretKey.copy")}
                </Button>
              </div>

              <Button
                data-testid="secret-key-saved-button"
                onClick={() => setPhase("confirm")}
                className="w-full"
              >
                {t("secretKey.saved")}
              </Button>
            </div>
          </Card>
        </div>
      </div>
    );
  }

  // Phase 3: confirm retrieval — key is NOT visible
  return (
    <div
      data-testid="save-secret-key-confirm-screen"
      className="flex flex-col h-full w-full bg-bg"
      style={{ position: "relative" }}
    >
      <div style={{ position: "absolute", inset: 0, opacity: 0.2, pointerEvents: "none" }}>
        <DotMatrix speed={0.2} />
      </div>
      <TitleBar />
      <div
        className="flex-1 flex justify-center overflow-y-auto"
        style={{ position: "relative", zIndex: 1, padding: "1rem" }}
      >
        <Card padding="lg" className="my-auto" style={{ width: "100%", maxWidth: 480 }}>
          <div className="flex flex-col gap-5">
            <div>
              <h1 className="text-base font-mono font-bold text-fg">
                {t("secretKey.confirmTitle")}
              </h1>
              <p
                className="text-xs mt-1 font-mono text-muted"
                style={{ lineHeight: 1.6 }}
              >
                {t("secretKey.confirmSubtitle")}
              </p>
            </div>

            <TextInput
              data-testid="secret-key-confirm-input"
              label={t("secretKey.inputLabel")}
              value={confirmInput}
              onChange={(v) => {
                setConfirmInput(v);
                setShowError(false);
              }}
              placeholder="A3-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX"
              error={showError && !matches ? t("secretKey.mismatch") : undefined}
            />

            <div className="flex flex-col gap-2">
              <Button
                data-testid="confirm-secret-key-button"
                onClick={handleConfirm}
                disabled={!matches}
                className="w-full"
              >
                {t("secretKey.confirmButton")}
              </Button>
              <Button
                data-testid="secret-key-back-button"
                onClick={() => setPhase("show")}
                variant="ghost"
                className="w-full"
              >
                {t("secretKey.showAgain")}
              </Button>
            </div>
          </div>
        </Card>
      </div>
    </div>
  );
};
