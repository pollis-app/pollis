import { errorMessage } from "../../utils/errorMessage";
import React, { useState, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { TitleBar } from "../Layout/TitleBar";
import { DotMatrix } from "../ui/DotMatrix";
import { Card } from "../ui/Card";
import { Button } from "../ui/Button";
import { InputOtp } from "../ui/InputOtp";
import * as api from "../../services/api";

interface PinCreateScreenProps {
  // Optional: set when the user is changing an existing PIN. The parent
  // owns the copy flip ("Set a PIN" vs "Create a new PIN").
  oldPin?: string;
  onCreated: () => void | Promise<void>;
  // Back button. Omit to hide (e.g. first-run migration where there's
  // nowhere safe to go back to).
  onCancel?: () => void;
  // Required rather than defaulted so the copy always comes from a caller that
  // has translated it — a literal default here would be untranslatable.
  headline: string;
  subline: string;
}

export const PinCreateScreen: React.FC<PinCreateScreenProps> = ({
  oldPin,
  onCreated,
  onCancel,
  headline,
  subline,
}) => {
  const { t } = useTranslation("auth");
  const [step, setStep] = useState<"enter" | "confirm">("enter");
  const [firstPin, setFirstPin] = useState("");
  const [confirmPin, setConfirmPin] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const hasAutoAdvancedRef = useRef(false);

  // When the first PIN reaches length 4, auto-advance to confirm.
  useEffect(() => {
    if (firstPin.length < 4) {
      hasAutoAdvancedRef.current = false;
      return;
    }
    if (hasAutoAdvancedRef.current) {
      return;
    }
    hasAutoAdvancedRef.current = true;
    setStep("confirm");
  }, [firstPin]);

  // Auto-submit when confirm reaches length 4.
  useEffect(() => {
    if (step !== "confirm" || confirmPin.length < 4 || isLoading) {
      return;
    }
    handleSubmit();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [confirmPin, step]);

  const handleSubmit = async () => {
    if (firstPin !== confirmPin) {
      setError(t("pinCreate.mismatch"));
      setStep("enter");
      setFirstPin("");
      setConfirmPin("");
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      await api.setPin(firstPin, oldPin);
      await onCreated();
    } catch (err) {
      setError(errorMessage(err));
      setStep("enter");
      setFirstPin("");
      setConfirmPin("");
    } finally {
      setIsLoading(false);
    }
  };

  const currentValue = step === "enter" ? firstPin : confirmPin;
  const setCurrent = step === "enter" ? setFirstPin : setConfirmPin;

  return (
    <div
      data-testid="pin-create-screen"
      className="flex flex-col h-full w-full bg-bg"
      style={{ position: "relative" }}
    >
      <div style={{ position: "absolute", inset: 0, opacity: 0.35, pointerEvents: "none" }}>
        <DotMatrix />
      </div>
      <TitleBar />
      <div
        className="flex-1 flex justify-center overflow-y-auto"
        style={{ position: "relative", zIndex: 1 }}
      >
        <Card padding="lg" className="my-auto" style={{ width: "100%", maxWidth: 360 }}>
          <div className="flex flex-col gap-5">
            <div>
              <h2 className="text-sm font-mono font-semibold text-fg">
                {step === "confirm" ? t("pinCreate.confirmHeadline") : headline}
              </h2>
              <p
                className="text-xs mt-1 font-mono text-muted"
                style={{ lineHeight: 1.5 }}
              >
                {step === "confirm" ? t("pinCreate.confirmSubline") : subline}
              </p>
            </div>

            {error && (
              <p
                data-testid="pin-create-error"
                className="text-xs font-mono text-danger"
              >
                {error}
              </p>
            )}

            <div>
              <InputOtp
                /* key forces a remount on step transition so the
                   focus / highlight state resets to the first slot */
                key={step}
                length={4}
                value={currentValue}
                onChange={(v) => {
                  setCurrent(v.replace(/\D/g, "").slice(0, 4));
                  setError(null);
                }}
                disabled={isLoading}
                autoFocus
                mask
              />
              <input
                data-testid="pin-create-input"
                type="hidden"
                value={currentValue}
                readOnly
              />
            </div>

            <Button
              data-testid="pin-create-submit"
              type="button"
              onClick={handleSubmit}
              isLoading={isLoading}
              loadingText={t("pinCreate.saving")}
              disabled={currentValue.length < 4}
              className="w-full"
            >
              {step === "confirm" ? t("pinCreate.save") : t("pinCreate.continue")}
            </Button>

            {onCancel && (
              <button
                data-testid="pin-create-cancel"
                onClick={onCancel}
                className="text-xs font-mono self-center text-muted border-0"
                style={{
                  background: "none",
                  cursor: "pointer",
                  padding: "0.25rem 0",
                }}
              >
                {t("common:actions.cancel")}
              </button>
            )}
          </div>
        </Card>
      </div>
    </div>
  );
};
