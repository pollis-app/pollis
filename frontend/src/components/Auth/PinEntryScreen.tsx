import React, { useState, useEffect, useRef } from "react";
import { TitleBar } from "../Layout/TitleBar";
import { DotMatrix } from "../ui/DotMatrix";
import { Card } from "../ui/Card";
import { Button } from "../ui/Button";
import { InputOtp } from "../ui/InputOtp";
import * as api from "../../services/api";

interface PinEntryScreenProps {
  userId: string;
  username?: string;
  onUnlocked: () => void | Promise<void>;
  // Routes to the Secret-Key recovery flow. After 10 wrong attempts the
  // backend wipes the wrapped blobs, so this is also how a user who
  // locked themselves out re-enrolls.
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
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      setPin("");
    } finally {
      setIsLoading(false);
    }
  };

  return (
    <div
      data-testid="pin-entry-screen"
      className="flex flex-col h-full w-full"
      style={{ background: "var(--c-bg)", position: "relative" }}
    >
      <div style={{ position: "absolute", inset: 0, opacity: 0.35, pointerEvents: "none" }}>
        <DotMatrix />
      </div>
      <TitleBar />
      <div
        className="flex-1 flex items-center justify-center"
        style={{ position: "relative", zIndex: 1 }}
      >
        <Card padding="lg" style={{ width: "100%", maxWidth: 360 }}>
          <div className="flex flex-col gap-5">
            <div>
              <h2 className="text-sm font-mono font-semibold" style={{ color: "var(--c-text)" }}>
                Enter PIN
              </h2>
              <p
                className="text-xs mt-1 font-mono"
                style={{ color: "var(--c-text-muted)", lineHeight: 1.5 }}
              >
                {username
                  ? <>Unlock Pollis as <span style={{ color: "var(--c-accent)" }}>{username}</span></>
                  : "Unlock Pollis on this device."}
              </p>
            </div>

            {error && (
              <p
                data-testid="pin-entry-error"
                className="text-xs font-mono"
                style={{ color: "#ff6b6b" }}
              >
                {error}
              </p>
            )}

            <div>
              <InputOtp
                length={4}
                value={pin}
                onChange={(v) => {
                  setPin(v.replace(/\D/g, "").slice(0, 4));
                  setError(null);
                }}
                disabled={isLoading}
                autoFocus
              />
              <input
                data-testid="pin-entry-input"
                type="hidden"
                value={pin}
                readOnly
              />
            </div>

            <Button
              data-testid="pin-entry-submit"
              type="button"
              onClick={handleSubmit}
              isLoading={isLoading}
              loadingText="Unlocking…"
              disabled={pin.length < 4}
              className="w-full"
            >
              Unlock
            </Button>

            <div className="flex flex-col gap-1 items-center">
              <button
                data-testid="pin-forgot-button"
                onClick={onForgotPin}
                className="text-xs font-mono"
                style={{
                  color: "var(--c-text-muted)",
                  background: "none",
                  border: "none",
                  cursor: "pointer",
                  padding: "0.25rem 0",
                }}
              >
                Forgot PIN? Recover with Secret Key
              </button>
              {onSwitchAccount && (
                <button
                  data-testid="pin-switch-account-button"
                  onClick={onSwitchAccount}
                  className="text-xs font-mono"
                  style={{
                    color: "var(--c-text-muted)",
                    background: "none",
                    border: "none",
                    cursor: "pointer",
                    padding: "0.25rem 0",
                  }}
                >
                  Use a different account
                </button>
              )}
            </div>
          </div>
        </Card>
      </div>
    </div>
  );
};
