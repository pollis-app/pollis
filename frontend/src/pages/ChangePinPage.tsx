import React, { useState, useEffect, useRef } from "react";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { InputOtp } from "../components/ui/InputOtp";
import { Button } from "../components/ui/Button";
import * as api from "../services/api";

type Step = "old" | "new" | "confirm";

export const ChangePinPage: React.FC = () => {
  const navigate = useNavigate();
  const [step, setStep] = useState<Step>("old");
  const [oldPin, setOldPin] = useState("");
  const [newPin, setNewPin] = useState("");
  const [confirmPin, setConfirmPin] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [done, setDone] = useState(false);
  const advancedRef = useRef<Record<Step, boolean>>({ old: false, new: false, confirm: false });

  // Auto-advance between steps when a 4-digit value is entered.
  useEffect(() => {
    if (step === "old" && oldPin.length === 4 && !advancedRef.current.old) {
      advancedRef.current.old = true;
      setStep("new");
    }
  }, [oldPin, step]);
  useEffect(() => {
    if (step === "new" && newPin.length === 4 && !advancedRef.current.new) {
      advancedRef.current.new = true;
      setStep("confirm");
    }
  }, [newPin, step]);
  useEffect(() => {
    if (step === "confirm" && confirmPin.length === 4 && !advancedRef.current.confirm) {
      advancedRef.current.confirm = true;
      handleSubmit();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [confirmPin, step]);

  const resetToOld = () => {
    setStep("old");
    setOldPin("");
    setNewPin("");
    setConfirmPin("");
    advancedRef.current = { old: false, new: false, confirm: false };
  };

  const handleSubmit = async () => {
    if (newPin !== confirmPin) {
      setError("New PINs don't match — try again");
      resetToOld();
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      await api.setPin(newPin, oldPin);
      setDone(true);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      resetToOld();
    } finally {
      setIsLoading(false);
    }
  };

  const currentValue = step === "old" ? oldPin : step === "new" ? newPin : confirmPin;
  const setCurrent = step === "old" ? setOldPin : step === "new" ? setNewPin : setConfirmPin;
  const heading = step === "old" ? "Current PIN" : step === "new" ? "New PIN" : "Confirm new PIN";

  return (
    <PageShell title="Change PIN" scrollable>
      <div
        className="flex flex-col gap-4 p-4 font-mono"
        data-testid="change-pin-page"
        style={{ color: "var(--c-text)" }}
      >
        {done ? (
          <div className="flex flex-col gap-4">
            <p className="text-xs" style={{ color: "var(--c-accent)" }}>
              PIN updated.
            </p>
            <Button
              data-testid="change-pin-done-button"
              onClick={() => navigate({ to: "/security" })}
            >
              Done
            </Button>
          </div>
        ) : (
          <div className="flex flex-col gap-5">
            <div>
              <h2 className="text-sm font-bold" style={{ color: "var(--c-accent)" }}>
                {heading}
              </h2>
              <p
                className="text-xs mt-1"
                style={{ color: "var(--c-text-muted)", lineHeight: 1.5 }}
              >
                {step === "old"
                  ? "Enter your current 4-digit PIN to continue."
                  : step === "new"
                  ? "Enter a new 4-digit PIN."
                  : "Enter the new PIN again to confirm."}
              </p>
            </div>

            {error && (
              <p
                data-testid="change-pin-error"
                className="text-xs"
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
                data-testid="change-pin-input"
                type="hidden"
                value={currentValue}
                readOnly
              />
            </div>

            <Button
              data-testid="change-pin-cancel-button"
              variant="ghost"
              onClick={() => navigate({ to: "/security" })}
            >
              Cancel
            </Button>
          </div>
        )}
      </div>
    </PageShell>
  );
};
