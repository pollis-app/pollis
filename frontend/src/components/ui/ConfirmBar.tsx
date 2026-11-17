import React from "react";
import { Button } from "./Button";
import { ComposerBarHeader } from "./ComposerBarHeader";

interface ConfirmBarProps {
  testId: string;
  heading: string;
  /** The consequence, in one sentence. */
  body: React.ReactNode;
  cancelLabel: string;
  cancelTestId: string;
  onCancel: () => void;
  confirmLabel: string;
  confirmTestId: string;
  onConfirm: () => void;
  isPending: boolean;
  pendingLabel: string;
}

/**
 * The one confirmation inside a conversation (#1968): it replaces the chat input bar. As on every
 * confirmation, the safe choice holds focus (the X), never the destructive action, so Enter can't
 * delete by accident; Escape, handled by the caller, also cancels. Elsewhere it is `ConfirmBlock`.
 */
export const ConfirmBar: React.FC<ConfirmBarProps> = ({
  testId,
  heading,
  body,
  cancelLabel,
  cancelTestId,
  onCancel,
  confirmLabel,
  confirmTestId,
  onConfirm,
  isPending,
  pendingLabel,
}) => (
  <div data-testid={testId}>
    <ComposerBarHeader
      heading={heading}
      onCancel={onCancel}
      cancelLabel={cancelLabel}
      cancelTestId={cancelTestId}
      focusCancel
    />
    <div className="flex items-center justify-between gap-4 bg-surface px-4 pb-3 pt-2">
      <p className="font-mono text-secondary text-dim">{body}</p>
      <Button
        data-testid={confirmTestId}
        variant="danger"
        onClick={onConfirm}
        isLoading={isPending}
        loadingText={pendingLabel}
      >
        {confirmLabel}
      </Button>
    </div>
  </div>
);
