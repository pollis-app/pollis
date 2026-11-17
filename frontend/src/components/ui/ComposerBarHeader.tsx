import React from "react";
import { X } from "lucide-react";
import { IconButton } from "./IconButton";

interface ComposerBarHeaderProps {
  heading: string;
  /** The X that leaves the bar; omitted for a bar that is a choice, not a cancellable step. */
  onCancel?: () => void;
  cancelLabel?: string;
  cancelTestId?: string;
  /** Focus the X when the bar opens: a confirmation's safe choice, never its action (#1968). */
  focusCancel?: boolean;
}

/** The heading row of a bar that replaces the chat input (edit, delete, a DM request). */
export const ComposerBarHeader: React.FC<ComposerBarHeaderProps> = ({
  heading,
  onCancel,
  cancelLabel,
  cancelTestId,
  focusCancel = false,
}) => (
  <div className="flex flex-shrink-0 items-center gap-2 border-t border-line bg-surface px-4 py-1.5">
    <span className="flex-1 font-mono text-meta uppercase tracking-widest text-muted">{heading}</span>
    {onCancel && (
      <IconButton
        data-testid={cancelTestId}
        onClick={onCancel}
        aria-label={cancelLabel ?? ""}
        autoFocus={focusCancel}
        className="flex-shrink-0"
      >
        <X size={20} aria-hidden="true" />
      </IconButton>
    )}
  </div>
);
