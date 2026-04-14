import React, { useMemo, useState } from "react";
import { TitleBar } from "../Layout/TitleBar";
import { DotMatrix } from "../ui/DotMatrix";
import { Card } from "../ui/Card";
import { Button } from "../ui/Button";
import { TextInput } from "../ui/TextInput";

interface SaveSecretKeyScreenProps {
  /// The freshly-generated Secret Key returned from `verify_otp`. Shown
  /// to the user once. We never store it on disk; once they confirm we
  /// pass control back to the parent.
  secretKey: string;
  /// Username of the account this key belongs to — used as a suffix on
  /// the downloaded emergency-kit filename so users with multiple
  /// accounts can tell their kits apart. Optional; omitted → unsuffixed
  /// filename.
  username?: string | null;
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
// underscore set so any exotic username (emoji, slashes, whitespace)
// can't produce an invalid filename on any OS.
function sanitizeForFilename(input: string): string {
  return input.replace(/[^a-zA-Z0-9_-]+/g, "_").replace(/^_+|_+$/g, "");
}

function downloadEmergencyKit(secretKey: string, username?: string | null) {
  const text = [
    "POLLIS — EMERGENCY KIT",
    "======================",
    "",
    "Your Secret Key is the only way to recover access to your account",
    "from a new device when you don't have any other Pollis device with",
    "you. Treat it like a master password.",
    "",
    "If you lose this key AND lose access to all of your devices,",
    "your account is unrecoverable. Pollis cannot reset it for you.",
    "",
    "  SECRET KEY:",
    "",
    `    ${secretKey}`,
    "",
    "Store this file somewhere safe (a password manager, encrypted",
    "backup, or printed and locked away). Anyone with this key + your",
    "email address can sign in as you on a new device.",
    "",
    `Generated: ${new Date().toISOString()}`,
    "",
  ].join("\n");

  const safeName = username ? sanitizeForFilename(username) : "";
  const filename = safeName
    ? `pollis-emergency-kit-${safeName}.txt`
    : "pollis-emergency-kit.txt";

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
  username,
  onConfirmed,
}) => {
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
        className="flex flex-col h-full w-full"
        style={{ background: "var(--c-bg)", position: "relative" }}
      >
        <div style={{ position: "absolute", inset: 0, opacity: 0.2, pointerEvents: "none" }}>
          <DotMatrix speed={0.2} />
        </div>
        <TitleBar />
        <div
          className="flex-1 flex items-center justify-center"
          style={{ position: "relative", zIndex: 1, padding: "1rem" }}
        >
          <Card padding="lg" style={{ width: "100%", maxWidth: 480 }}>
            <div className="flex flex-col gap-5">
              <div>
                <h1
                  className="text-base font-mono font-bold"
                  style={{ color: "#ff6b6b" }}
                >
                  Read before continuing
                </h1>
                <p
                  className="text-xs mt-2 font-mono"
                  style={{ color: "var(--c-text)", lineHeight: 1.6 }}
                >
                  You will be presented with a <strong>Secret Key</strong> — the
                  only way to sign in on a new device without access to an
                  existing one.
                </p>
                <p
                  className="text-xs mt-3 font-mono"
                  style={{ color: "var(--c-text)", lineHeight: 1.6 }}
                >
                  It is shown <strong>once</strong> and never stored on a server.
                  Lose it and every device, and the account is unrecoverable.
                </p>
                <p
                  className="text-xs mt-3 font-mono"
                  style={{ color: "var(--c-text-muted)", lineHeight: 1.6 }}
                >
                  Have a password manager open, or be ready to print or write
                  it down somewhere safe.
                </p>
              </div>
              <Button
                data-testid="save-secret-key-acknowledge-button"
                onClick={() => setPhase("show")}
                className="w-full"
              >
                I understand, show me the key
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
        className="flex flex-col h-full w-full"
        style={{ background: "var(--c-bg)", position: "relative" }}
      >
        <div style={{ position: "absolute", inset: 0, opacity: 0.2, pointerEvents: "none" }}>
          <DotMatrix speed={0.2} />
        </div>
        <TitleBar />
        <div
          className="flex-1 flex items-center justify-center"
          style={{ position: "relative", zIndex: 1, padding: "1rem", overflowY: "auto" }}
        >
          <Card padding="lg" style={{ width: "100%", maxWidth: 480 }}>
            <div className="flex flex-col gap-5">
              <div>
                <h1
                  className="text-base font-mono font-bold"
                  style={{ color: "var(--c-accent)" }}
                >
                  Your Secret Key
                </h1>
                <p
                  className="text-xs mt-1 font-mono"
                  style={{ color: "var(--c-text-muted)" }}
                >
                  Save this somewhere safe. You will not see it again.
                </p>
              </div>

              <div
                data-testid="secret-key-display"
                className="font-mono text-sm select-all"
                style={{
                  background: "var(--c-surface)",
                  border: "2px solid var(--c-accent)",
                  borderRadius: "0.5rem",
                  padding: "1rem",
                  color: "var(--c-accent)",
                  wordBreak: "break-all",
                  textAlign: "center",
                  letterSpacing: "0.05em",
                }}
              >
                {secretKey}
              </div>

              <div className="flex flex-col gap-2">
                <Button
                  data-testid="copy-secret-key-button"
                  onClick={() => {
                    navigator.clipboard
                      .writeText(secretKey)
                      .then(() => {
                        setCopied(true);
                        window.setTimeout(() => setCopied(false), 2000);
                      })
                      .catch(() => {});
                  }}
                  variant="ghost"
                  className="w-full"
                >
                  {copied ? "Copied" : "Copy to clipboard"}
                </Button>
                <Button
                  data-testid="download-secret-key-button"
                  onClick={() => {
                    downloadEmergencyKit(secretKey, username);
                    setDownloaded(true);
                    window.setTimeout(() => setDownloaded(false), 2000);
                  }}
                  variant="ghost"
                  className="w-full"
                >
                  {downloaded ? "Downloaded" : "Download Emergency Kit (.txt)"}
                </Button>
              </div>

              <Button
                data-testid="secret-key-saved-button"
                onClick={() => setPhase("confirm")}
                className="w-full"
              >
                I've saved it — continue
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
      className="flex flex-col h-full w-full"
      style={{ background: "var(--c-bg)", position: "relative" }}
    >
      <div style={{ position: "absolute", inset: 0, opacity: 0.2, pointerEvents: "none" }}>
        <DotMatrix speed={0.2} />
      </div>
      <TitleBar />
      <div
        className="flex-1 flex items-center justify-center"
        style={{ position: "relative", zIndex: 1, padding: "1rem" }}
      >
        <Card padding="lg" style={{ width: "100%", maxWidth: 480 }}>
          <div className="flex flex-col gap-5">
            <div>
              <h1
                className="text-base font-mono font-bold"
                style={{ color: "var(--c-text)" }}
              >
                Confirm your Secret Key
              </h1>
              <p
                className="text-xs mt-1 font-mono"
                style={{ color: "var(--c-text-muted)", lineHeight: 1.6 }}
              >
                Paste the key you just saved to prove you can retrieve it.
              </p>
            </div>

            <TextInput
              data-testid="secret-key-confirm-input"
              label="Secret Key"
              value={confirmInput}
              onChange={(v) => {
                setConfirmInput(v);
                setShowError(false);
              }}
              placeholder="A3-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX"
              error={showError && !matches ? "Doesn't match — try again" : undefined}
            />

            <div className="flex flex-col gap-2">
              <Button
                data-testid="confirm-secret-key-button"
                onClick={handleConfirm}
                disabled={!matches}
                className="w-full"
              >
                Confirm
              </Button>
              <Button
                data-testid="secret-key-back-button"
                onClick={() => setPhase("show")}
                variant="ghost"
                className="w-full"
              >
                Show the key again
              </Button>
            </div>
          </div>
        </Card>
      </div>
    </div>
  );
};
