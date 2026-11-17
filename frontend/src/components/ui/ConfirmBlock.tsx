import React from "react";
import { Button } from "./Button";
import { FormActions, type FormCancel } from "./FormActions";

interface ConfirmBlockProps {
  /** The question, naming what it acts on ("Leave Design team?"). */
  question: React.ReactNode;
  /** The consequence in one plain sentence ("You will need a new invite to rejoin."). */
  consequence?: React.ReactNode;
  /** A field the confirmation needs (a typed name), between the sentences and the actions. */
  children?: React.ReactNode;
  error?: string | null;
  errorTestId?: string;
  confirmLabel: string;
  onConfirm: () => void;
  isPending?: boolean;
  pendingLabel?: string;
  confirmDisabled?: boolean;
  confirmTestId?: string;
  /** Cancel takes focus on mount unless `children` holds a field that does. */
  cancel: FormCancel;
  testId?: string;
}

/**
 * The one confirmation (#1968) in a page column: the question and its consequence, then the
 * destructive action (the page's only `danger` button) and Cancel. Cancel, never the action, is
 * the default focus. `ConfirmPage` is this as a page; `ConfirmBar` is its chat-input-bar twin.
 */
export const ConfirmBlock: React.FC<ConfirmBlockProps> = ({
  question,
  consequence,
  children,
  error,
  errorTestId,
  confirmLabel,
  onConfirm,
  isPending = false,
  pendingLabel,
  confirmDisabled = false,
  confirmTestId,
  cancel,
  testId,
}) => (
  <div data-testid={testId} className="flex flex-col gap-5">
    <div className="flex flex-col gap-1">
      <p className="text-body text-fg">{question}</p>
      {consequence && <p className="text-secondary text-dim">{consequence}</p>}
    </div>
    {children}
    {error && (
      <p data-testid={errorTestId} role="alert" className="text-secondary text-danger">
        {error}
      </p>
    )}
    <FormActions cancel={{ autoFocus: !children, ...cancel }}>
      <Button
        data-testid={confirmTestId}
        variant="danger"
        onClick={onConfirm}
        disabled={confirmDisabled || isPending}
        isLoading={isPending}
        loadingText={pendingLabel}
      >
        {confirmLabel}
      </Button>
    </FormActions>
  </div>
);
