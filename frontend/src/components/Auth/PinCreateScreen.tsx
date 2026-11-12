import { errorMessage } from "../../utils/errorMessage";
import { PinField } from "./PinField";
import React, { useState, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { BackdropScreen } from "./BackdropScreen";
import { AuthCard } from "./AuthCard";
import { Button } from "../ui/Button";
import * as api from "../../services/api";

interface PinCreateScreenProps {
  // Set when changing an existing PIN.
  oldPin?: string;
  onCreated: () => void | Promise<void>;
  // Omit to hide.
  onCancel?: () => void;
  // Required: a literal default would be untranslatable.
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
    <BackdropScreen data-testid="pin-create-screen" backdropOpacity={0.35}>
      <AuthCard>
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

          {/* remount per step to reset focus */}
          <PinField
            key={step}
            value={currentValue}
            onChange={(next) => {
              setCurrent(next);
              setError(null);
            }}
            disabled={isLoading}
            data-testid="pin-create-input"
          />

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
      </AuthCard>
    </BackdropScreen>
  );
};
