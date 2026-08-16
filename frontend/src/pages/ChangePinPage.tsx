import { errorMessage } from "../utils/errorMessage";
import React, { useState, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { InputOtp } from "../components/ui/InputOtp";
import { Button } from "../components/ui/Button";
import * as api from "../services/api";

type Step = "old" | "new" | "confirm";

export const ChangePinPage: React.FC = () => {
  const { t } = useTranslation("settings");
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
      setError(t("changePin.mismatch"));
      resetToOld();
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      await api.setPin(newPin, oldPin);
      setDone(true);
    } catch (err) {
      setError(errorMessage(err));
      resetToOld();
    } finally {
      setIsLoading(false);
    }
  };

  const currentValue = step === "old" ? oldPin : step === "new" ? newPin : confirmPin;
  const setCurrent = step === "old" ? setOldPin : step === "new" ? setNewPin : setConfirmPin;
  const heading =
    step === "old"
      ? t("changePin.headingOld")
      : step === "new"
      ? t("changePin.headingNew")
      : t("changePin.headingConfirm");

  return (
    <PageShell title={t("changePin.title")} scrollable>
      <div className="flex justify-center px-6 py-8">
      <div
        className="flex flex-col gap-4 w-full max-w-md font-mono text-fg"
        data-testid="change-pin-page"
      >
        {done ? (
          <div className="flex flex-col gap-4">
            <p className="text-xs text-accent">
              {t("changePin.updated")}
            </p>
            <Button
              data-testid="change-pin-done-button"
              onClick={() => navigate({ to: "/security" })}
            >
              {t("changePin.doneButton")}
            </Button>
          </div>
        ) : (
          <div className="flex flex-col gap-5">
            <div>
              <h2 className="text-sm font-bold text-accent">
                {heading}
              </h2>
              <p
                className="text-xs mt-1 text-muted"
                style={{ lineHeight: 1.5 }}
              >
                {step === "old"
                  ? t("changePin.hintOld")
                  : step === "new"
                  ? t("changePin.hintNew")
                  : t("changePin.hintConfirm")}
              </p>
            </div>

            {error && (
              <p
                data-testid="change-pin-error"
                className="text-xs text-danger"
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
                mask
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
              {t("common:actions.cancel")}
            </Button>
          </div>
        )}
      </div>
      </div>
    </PageShell>
  );
};
