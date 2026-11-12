import { errorMessage } from "../../utils/errorMessage";
import { PinField } from "./PinField";
import React, { useState, useEffect, useRef } from "react";
import { Trans, useTranslation } from "react-i18next";
import { BackdropScreen } from "./BackdropScreen";
import { AuthCard } from "./AuthCard";
import { Button } from "../ui/Button";
import * as api from "../../services/api";

interface PinEntryScreenProps {
  userId: string;
  username?: string;
  onUnlocked: () => void | Promise<void>;
  // Secret-Key recovery; also the way back after 10 wrong attempts wipe the blobs.
  onForgotPin: () => void;
  onSwitchAccount?: () => void;
}

export const PinEntryScreen: React.FC<PinEntryScreenProps> = ({
  userId,
  username,
  onUnlocked,
  onForgotPin,
  onSwitchAccount,
}) => {
  const { t } = useTranslation("auth");
  const [pin, setPin] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const hasAutoSubmittedRef = useRef(false);

  useEffect(() => {
    if (pin.length < 4) {
      hasAutoSubmittedRef.current = false;
      return;
    }
    if (hasAutoSubmittedRef.current || isLoading) {
      return;
    }
    hasAutoSubmittedRef.current = true;
    handleSubmit();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pin]);

  const handleSubmit = async () => {
    if (pin.length < 4) {
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      await api.unlockWithPin(userId, pin);
      await onUnlocked();
    } catch (err) {
      const msg = errorMessage(err);
      setError(msg);
      setPin("");
    } finally {
      setIsLoading(false);
    }
  };

  return (
    <BackdropScreen data-testid="pin-entry-screen" backdropOpacity={0.35}>
      <AuthCard>
        <div className="flex flex-col gap-5">
          <div>
            <h2 className="text-sm font-mono font-semibold text-fg">
              {t("pinEntry.headline")}
            </h2>
            <p
              className="text-xs mt-1 font-mono text-muted"
              style={{ lineHeight: 1.5 }}
            >
              {username
                ? (
                  <Trans
                    t={t}
                    i18nKey="pinEntry.unlockAs"
                    values={{ username }}
                    components={{ name: <span className="text-accent" /> }}
                  />
                )
                : t("pinEntry.unlockDevice")}
            </p>
          </div>

          {error && (
            <p
              data-testid="pin-entry-error"
              className="text-xs font-mono text-danger"
            >
              {error}
            </p>
          )}

          <PinField
            value={pin}
            onChange={(next) => {
              setPin(next);
              setError(null);
            }}
            disabled={isLoading}
            data-testid="pin-entry-input"
          />

          <Button
            data-testid="pin-entry-submit"
            type="button"
            onClick={handleSubmit}
            isLoading={isLoading}
            loadingText={t("pinEntry.unlocking")}
            disabled={pin.length < 4}
            className="w-full"
          >
            {t("pinEntry.unlock")}
          </Button>

          <div className="flex flex-col gap-1 items-center">
            <button
              data-testid="pin-forgot-button"
              onClick={onForgotPin}
              className="text-xs font-mono text-muted border-0"
              style={{
                background: "none",
                cursor: "pointer",
                padding: "0.25rem 0",
              }}
            >
              {t("pinEntry.forgot")}
            </button>
            {onSwitchAccount && (
              <button
                data-testid="pin-switch-account-button"
                onClick={onSwitchAccount}
                className="text-xs font-mono text-muted border-0"
                style={{
                  background: "none",
                  cursor: "pointer",
                  padding: "0.25rem 0",
                }}
              >
                {t("pinEntry.switchAccount")}
              </button>
            )}
          </div>
        </div>
      </AuthCard>
    </BackdropScreen>
  );
};
