import React, { useState } from "react";
import { EmailOTPAuth } from "./EmailOTPAuth";
import { TitleBar } from "../Layout/TitleBar";
import { DotMatrix } from "../ui/DotMatrix";
import { Card } from "../ui/Card";
import { Button } from "../ui/Button";
import type { AccountInfo } from "../../types";
import * as api from "../../services/api";

interface LoginScreenProps {
  knownAccounts: AccountInfo[];
  onAuthSuccess: (result: api.AuthResult) => void | Promise<void>;
  onWipeComplete: () => void;
}

export const LoginScreen: React.FC<LoginScreenProps> = ({
  knownAccounts,
  onAuthSuccess,
  onWipeComplete,
}) => {
  const [view, setView] = useState<"login" | "wipe">("login");
  const [authStep, setAuthStep] = useState<"email" | "otp">("email");
  const [prefillEmail, setPrefillEmail] = useState<string | undefined>(undefined);
  const [prefillNonce, setPrefillNonce] = useState(0);
  const [isWiping, setIsWiping] = useState(false);

  return (
    <div
      data-testid="auth-screen"
      className="flex flex-col h-full w-full"
      style={{ background: "var(--c-bg)", position: "relative" }}
    >
      <div style={{ position: "absolute", inset: 0, opacity: 0.35, pointerEvents: "none" }}>
        <DotMatrix />
      </div>

      <TitleBar />

      <div className="flex-1 flex items-center justify-center" style={{ position: "relative", zIndex: 1 }}>
        <Card padding="lg" style={{ width: "100%", maxWidth: 360 }}>
          {view === "wipe" ? (
            <div data-testid="wipe-confirm-section" className="flex flex-col gap-5">
              <div>
                <h2 className="text-sm font-mono font-semibold" style={{ color: "var(--c-text)" }}>
                  Delete local profiles
                </h2>
                <p
                  className="text-xs mt-2 font-mono"
                  style={{ color: "#ff6b6b", lineHeight: 1.5 }}
                >
                  This will delete all local databases, keys, and saved
                  accounts on this device. Your remote account is not affected.
                </p>
              </div>
              <div className="flex gap-2">
                <Button
                  data-testid="wipe-confirm-button"
                  variant="danger"
                  className="flex-1"
                  isLoading={isWiping}
                  loadingText="Wiping..."
                  onClick={async () => {
                    setIsWiping(true);
                    try {
                      await api.wipeLocalData();
                      onWipeComplete();
                      setView("login");
                    } catch (err) {
                      console.error("[wipe]", err);
                    } finally {
                      setIsWiping(false);
                    }
                  }}
                >
                  Wipe all local data
                </Button>
                <Button
                  data-testid="wipe-cancel-button"
                  variant="ghost"
                  className="flex-1"
                  disabled={isWiping}
                  onClick={() => setView("login")}
                >
                  Cancel
                </Button>
              </div>
            </div>
          ) : (
            <div className="flex flex-col gap-5">
              <div>
                <h1 className="text-base font-mono font-bold" style={{ color: "var(--c-accent)" }}>
                  Pollis.
                </h1>
                <p className="text-xs mt-1 font-mono" style={{ color: "var(--c-text-muted)" }}>
                  Enter your email to continue
                </p>
              </div>

              {/* Known accounts row — most recent 3, sorted by last_seen desc */}
              {knownAccounts.length > 0 && authStep === "email" && (
                <div className="flex flex-col gap-1">
                  <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                    Previously signed in:
                  </p>
                  <div className="flex flex-wrap gap-2">
                    {[...knownAccounts]
                      .sort((a, b) => (a.last_seen < b.last_seen ? 1 : -1))
                      .slice(0, 3)
                      .map((account) => (
                      <button
                        key={account.user_id}
                        data-testid={`known-account-chip-${account.user_id}`}
                        onClick={() => {
                          if (account.email) {
                            setPrefillEmail(account.email);
                            setPrefillNonce((n) => n + 1);
                          }
                        }}
                        disabled={!account.email}
                        className="flex items-center gap-1 px-2 py-1 font-mono text-xs transition-colors"
                        style={{
                          background: "var(--c-surface)",
                          border: "2px solid var(--c-border)",
                          borderRadius: "0.5rem",
                          color: "var(--c-text-dim)",
                          cursor: account.email ? "pointer" : "default",
                        }}
                        onMouseEnter={(e) => {
                          if (!account.email) {
                            return;
                          }
                          (e.currentTarget as HTMLButtonElement).style.borderColor = "var(--c-accent)";
                          (e.currentTarget as HTMLButtonElement).style.color = "var(--c-text)";
                        }}
                        onMouseLeave={(e) => {
                          (e.currentTarget as HTMLButtonElement).style.borderColor = "var(--c-border)";
                          (e.currentTarget as HTMLButtonElement).style.color = "var(--c-text-dim)";
                        }}
                      >
                        <span>{account.username}</span>
                      </button>
                    ))}
                  </div>
                </div>
              )}

              <EmailOTPAuth
                onSuccess={onAuthSuccess}
                prefillEmail={prefillEmail}
                prefillNonce={prefillNonce}
                onStepChange={setAuthStep}
              />

              {authStep === "email" && (
                <button
                  data-testid="wipe-local-data-button"
                  onClick={() => setView("wipe")}
                  className="text-xs font-mono self-center"
                  style={{
                    color: "var(--c-text-muted)",
                    background: "none",
                    border: "none",
                    cursor: "pointer",
                    padding: "0.25rem 0",
                    marginTop: "1rem",
                  }}
                  onMouseEnter={(e) => {
                    (e.currentTarget as HTMLButtonElement).style.color = "#ff6b6b";
                  }}
                  onMouseLeave={(e) => {
                    (e.currentTarget as HTMLButtonElement).style.color = "var(--c-text-muted)";
                  }}
                >
                  Delete local profiles
                </button>
              )}
            </div>
          )}
        </Card>
      </div>
    </div>
  );
};
