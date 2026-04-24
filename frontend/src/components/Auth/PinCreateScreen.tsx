import React, { useState, useEffect, useRef } from "react";
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
  headline?: string;
  subline?: string;
}

export const PinCreateScreen: React.FC<PinCreateScreenProps> = ({
  oldPin,
  onCreated,
  onCancel,
  headline = "Set a PIN",
  subline = "4 digits. You'll use it to unlock Pollis on this device.",
}) => {
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
      setError("PINs don't match — try again");
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
      setError(err instanceof Error ? err.message : String(err));
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
                {step === "confirm" ? "Confirm PIN" : headline}
              </h2>
              <p
                className="text-xs mt-1 font-mono"
                style={{ color: "var(--c-text-muted)", lineHeight: 1.5 }}
              >
                {step === "confirm" ? "Enter the PIN again to confirm." : subline}
              </p>
            </div>

            {error && (
              <p
                data-testid="pin-create-error"
                className="text-xs font-mono"
                style={{ color: "#ff6b6b" }}
              >
                {error}
              </p>
            )}

            <div>
              <InputOtp
                length={4}
                value={currentValue}
                onChange={(v) => {
                  setCurrent(v.replace(/\D/g, "").slice(0, 4));
                  setError(null);
                }}
                disabled={isLoading}
                autoFocus
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
              loadingText="Saving…"
              disabled={currentValue.length < 4}
              className="w-full"
            >
              {step === "confirm" ? "Save PIN" : "Continue"}
            </Button>

            {onCancel && (
              <button
                data-testid="pin-create-cancel"
                onClick={onCancel}
                className="text-xs font-mono self-center"
                style={{
                  color: "var(--c-text-muted)",
                  background: "none",
                  border: "none",
                  cursor: "pointer",
                  padding: "0.25rem 0",
                }}
              >
                Cancel
              </button>
            )}
          </div>
        </Card>
      </div>
    </div>
  );
};
