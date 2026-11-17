import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { EmailOTPAuth } from "./EmailOTPAuth";
import { BackdropScreen } from "./BackdropScreen";
import { AuthCard } from "./AuthCard";
import { Button } from "../ui/Button";
import { Chip } from "../ui/Chip";
import type { AccountInfo } from "../../types";
import * as api from "../../services/api";

interface LoginScreenProps {
  knownAccounts: AccountInfo[];
  onAuthSuccess: (result: api.AuthResult) => void | Promise<void>;
  onWipeComplete: () => void;
  /** Why the session ended, when not the user's choice (#1338). */
  notice?: string | null;
}

export const LoginScreen: React.FC<LoginScreenProps> = ({
  knownAccounts,
  onAuthSuccess,
  onWipeComplete,
  notice,
}) => {
  const { t } = useTranslation("auth");
  const [view, setView] = useState<"login" | "wipe">("login");
  const [authStep, setAuthStep] = useState<"email" | "otp">("email");
  const [prefillEmail, setPrefillEmail] = useState<string | undefined>(undefined);
  const [prefillNonce, setPrefillNonce] = useState(0);
  const [isWiping, setIsWiping] = useState(false);

  return (
    <BackdropScreen data-testid="auth-screen" backdropOpacity={0.35}>
      <AuthCard>
        {notice ? (
          <p
            data-testid="auth-session-notice"
            role="status"
            className="text-secondary font-mono text-fg mb-4"
            style={{ lineHeight: 1.5 }}
          >
            {notice}
          </p>
        ) : null}
        {view === "wipe" ? (
          <div data-testid="wipe-confirm-section" className="flex flex-col gap-5">
            <div>
              <h2 className="text-body font-mono font-semibold text-fg">
                {t("wipe.title")}
              </h2>
              <p
                className="text-secondary mt-2 font-mono text-danger"
                style={{ lineHeight: 1.5 }}
              >
                {t("wipe.warning")}
              </p>
            </div>
            <div className="flex gap-2">
              <Button
                data-testid="wipe-confirm-button"
                variant="danger"
                className="flex-1"
                isLoading={isWiping}
                loadingText={t("wipe.wiping")}
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
                {t("wipe.confirm")}
              </Button>
              <Button
                data-testid="wipe-cancel-button"
                variant="ghost"
                className="flex-1"
                disabled={isWiping}
                onClick={() => setView("login")}
              >
                {t("common:actions.cancel")}
              </Button>
            </div>
          </div>
        ) : (
          <div className="flex flex-col gap-5">
            <div>

              <p className="text-secondary mt-1 font-mono text-accent" data-testid="login-prompt">
                {authStep === "otp" ? t("login.otpPrompt") : t("login.prompt")}
              </p>
            </div>

            {knownAccounts.length > 0 && authStep === "email" && (
              <div className="flex flex-col gap-1">
                <p className="text-secondary font-mono text-muted">
                  {t("login.previousAccounts")}
                </p>
                <div className="flex flex-wrap gap-2">
                  {[...knownAccounts]
                    .sort((a, b) => (a.last_seen < b.last_seen ? 1 : -1))
                    .slice(0, 3)
                    .map((account) => (
                      <Chip
                        key={account.user_id}
                        data-testid={`known-account-chip-${account.user_id}`}
                        onClick={() => {
                          if (account.email) {
                            setPrefillEmail(account.email);
                            setPrefillNonce((n) => n + 1);
                          }
                        }}
                        disabled={!account.email}
                      >
                        <span>{account.username}</span>
                      </Chip>
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
              <Button
                variant="ghost"
                data-testid="wipe-local-data-button"
                onClick={() => setView("wipe")}
                className="self-center mt-4"
              >
                {t("wipe.open")}
              </Button>
            )}
          </div>
        )}
      </AuthCard>
    </BackdropScreen>
  );
};
