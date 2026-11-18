import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { PageColumn } from "../ui/PageColumn";
import { FormLead } from "../ui/FormLead";
import { FormActions, type FormCancel } from "../ui/FormActions";
import { Button } from "../ui/Button";
import { CodeSnippet } from "../ui/CodeSnippet";
import { SasCodeInput } from "../ui/SasCodeInput";
import { SAS_LENGTH } from "../../utils/enrollmentSas";
import { botErrorText } from "./botErrorText";

interface BotPairingFormProps {
  leadTitle: string;
  lead: string;
  /** The command the admin runs on the bot's machine; it prints the code. */
  command: string;
  /** Fields before the code (the new bot's name). */
  children?: React.ReactNode;
  /** False while those fields are incomplete. */
  ready?: boolean;
  submitLabel: string;
  submittingLabel: string;
  failedLabel: string;
  /** Resolves when the host is paired; rejects with the core's error. */
  onSubmit: (code: string) => Promise<void>;
  cancel: FormCancel;
  testId: string;
}

/**
 * Pairing a machine with a bot (#1182), as a new bot or as another host of one: the command to
 * run there, what that machine can do, and the code it prints, typed by the admin (never
 * pre-filled; the core checks it against the key it fetches). Busy from submit until it lands.
 */
export const BotPairingForm: React.FC<BotPairingFormProps> = ({
  leadTitle,
  lead,
  command,
  children,
  ready = true,
  submitLabel,
  submittingLabel,
  failedLabel,
  onSubmit,
  cancel,
  testId,
}) => {
  const { t } = useTranslation("channels");
  const [code, setCode] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const canSubmit = ready && code.length === SAS_LENGTH && !pending;

  const submit = async (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    if (!canSubmit) {
      return;
    }
    setPending(true);
    setError(null);
    try {
      await onSubmit(code);
    } catch (err) {
      setError(botErrorText(err, t, failedLabel));
      // A wrong or stale code means reading the other screen again.
      setCode("");
      setPending(false);
    }
  };

  return (
    <PageColumn as="form" onSubmit={submit} data-testid={testId}>
      <FormLead title={leadTitle}>{lead}</FormLead>
      <div className="flex flex-col gap-1.5">
        <p className="text-secondary font-medium text-fg">{t("bots.pairing.commandLabel")}</p>
        <CodeSnippet value={command} data-testid="bot-pair-command" copyTestId="bot-pair-command-copy" />
      </div>
      <p data-testid="bot-machine-notice" className="text-secondary text-dim">
        {t("bots.pairing.machineNotice")}
      </p>
      {children}
      <SasCodeInput
        data-testid="bot-pair-code"
        label={t("bots.pairing.codeLabel")}
        description={t("auth:approval.codeHint", { count: SAS_LENGTH })}
        value={code}
        onChange={(next) => {
          setError(null);
          setCode(next);
        }}
        disabled={pending}
      />
      {error && (
        <p data-testid="bot-pair-error" role="alert" className="text-secondary text-danger">
          {error}
        </p>
      )}
      <FormActions cancel={{ disabled: pending, ...cancel }}>
        <Button
          type="submit"
          data-testid="bot-pair-submit"
          disabled={!canSubmit}
          isLoading={pending}
          loadingText={submittingLabel}
        >
          {submitLabel}
        </Button>
      </FormActions>
    </PageColumn>
  );
};
