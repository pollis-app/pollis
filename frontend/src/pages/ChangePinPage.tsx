import { errorMessage } from "../utils/errorMessage";
import { PinField } from "../components/Auth/PinField";
import React, { useState, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { useFinishFlow } from "../hooks/useFinishFlow";
import { PageShell } from "../components/Layout/PageShell";
import { PageColumn } from "../components/ui/PageColumn";
import { FormLead } from "../components/ui/FormLead";
import { FormActions } from "../components/ui/FormActions";
import { Button } from "../components/ui/Button";
import * as api from "../services/api";

type Step = "old" | "new" | "confirm";

export const ChangePinPage: React.FC = () => {
  const { t } = useTranslation("settings");
  const { finish } = useFinishFlow();
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
      <PageColumn data-testid="change-pin-page" className="font-mono text-fg">
        {done ? (
          <>
            <p className="text-secondary text-accent">
              {t("changePin.updated")}
            </p>
            <FormActions>
              <Button
                data-testid="change-pin-done-button"
                onClick={() => finish({ to: "/security" })}
              >
                {t("changePin.doneButton")}
              </Button>
            </FormActions>
          </>
        ) : (
          <>
            <FormLead title={heading}>
              {step === "old"
                ? t("changePin.hintOld")
                : step === "new"
                ? t("changePin.hintNew")
                : t("changePin.hintConfirm")}
            </FormLead>

            {error && (
              <p
                data-testid="change-pin-error"
                className="text-secondary text-danger"
              >
                {error}
              </p>
            )}

            <PinField
              value={currentValue}
              onChange={(next) => {
                setCurrent(next);
                setError(null);
              }}
              disabled={isLoading}
              data-testid="change-pin-input"
            />

            <FormActions
              cancel={{
                testId: "change-pin-cancel-button",
                onClick: () => finish({ to: "/security" }),
              }}
            />
          </>
        )}
      </PageColumn>
    </PageShell>
  );
};
