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

function downloadEmergencyKit(secretKey: string) {
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

  const blob = new Blob([text], { type: "text/plain" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = "pollis-emergency-kit.txt";
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

export const SaveSecretKeyScreen: React.FC<SaveSecretKeyScreenProps> = ({
  secretKey,
  onConfirmed,
}) => {
  const [acknowledged, setAcknowledged] = useState(false);
  const [confirmInput, setConfirmInput] = useState("");
  const [showError, setShowError] = useState(false);

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

  if (!acknowledged) {
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
                  Important — read before you continue
                </h1>
                <p
                  className="text-xs mt-2 font-mono"
                  style={{ color: "var(--c-text)", lineHeight: 1.6 }}
                >
                  We're about to show you a <strong>Secret Key</strong>. This is the
                  only way to sign in on a new device when you don't have any of
                  your existing devices with you.
                </p>
                <p
                  className="text-xs mt-3 font-mono"
                  style={{ color: "var(--c-text)", lineHeight: 1.6 }}
                >
                  We will <strong>only show it once</strong>. Pollis never stores
                  it on our servers. If you lose it AND lose access to all of your
                  devices, your account is permanently unrecoverable. We cannot
                  reset it. You will lose access to every group and every message.
                </p>
                <p
                  className="text-xs mt-3 font-mono"
                  style={{ color: "var(--c-text-muted)", lineHeight: 1.6 }}
                >
                  Have a password manager (1Password, Bitwarden, Apple Passwords)
                  open and ready, or be prepared to print or write it down and
                  store it somewhere safe.
                </p>
              </div>
              <Button
                data-testid="save-secret-key-acknowledge-button"
                onClick={() => setAcknowledged(true)}
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
                  navigator.clipboard.writeText(secretKey).catch(() => {});
                }}
                variant="ghost"
                className="w-full"
              >
                Copy to clipboard
              </Button>
              <Button
                data-testid="download-secret-key-button"
                onClick={() => downloadEmergencyKit(secretKey)}
                variant="ghost"
                className="w-full"
              >
                Download Emergency Kit (.txt)
              </Button>
            </div>

            <div
              style={{
                borderTop: "1px solid var(--c-border)",
                paddingTop: "1rem",
              }}
            >
              <p
                className="text-xs font-mono mb-2"
                style={{ color: "var(--c-text)" }}
              >
                Type your Secret Key below to confirm you've saved it:
              </p>
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
            </div>

            <Button
              data-testid="confirm-secret-key-button"
              onClick={handleConfirm}
              disabled={!matches}
              className="w-full"
            >
              I've saved my Secret Key
            </Button>
          </div>
        </Card>
      </div>
    </div>
  );
};
