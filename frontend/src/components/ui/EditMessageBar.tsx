import React, { useEffect, useRef, useState } from "react";
import { X } from "lucide-react";

interface EditMessageBarProps {
  heading: string;
  cancelLabel: string;
  hint: string;
  value: string;
  onChange: (value: string) => void;
  /** Enter (without Shift) saves. Escape is the caller's: it is handled at
   * the page level in capture phase, ahead of the window navigation
   * handler. */
  onSave: () => void;
  onCancel: () => void;
  isSaving: boolean;
  testId: string;
  inputTestId: string;
  cancelTestId: string;
}

/**
 * The bar that replaces the composer while a message is being edited — the
 * channel/DM log and the Vault share it, so editing looks and behaves the
 * same in both (heading row, focus-inverted textarea, one-line key hint).
 *
 * Mounting focuses the textarea with the caret at the end; give it a `key`
 * of the edited message id so switching to another message remounts it.
 */
export const EditMessageBar: React.FC<EditMessageBarProps> = ({
  heading,
  cancelLabel,
  hint,
  value,
  onChange,
  onSave,
  onCancel,
  isSaving,
  testId,
  inputTestId,
  cancelTestId,
}) => {
  const [focused, setFocused] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    const el = textareaRef.current;
    if (!el) {
      return;
    }
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
  }, []);

  return (
    <div data-testid={testId}>
      <div className="flex items-center gap-2 px-4 py-1.5 flex-shrink-0 border-t border-line bg-surface">
        <span className="flex-1 text-2xs font-mono uppercase tracking-widest text-muted">
          {heading}
        </span>
        <button
          data-testid={cancelTestId}
          onClick={onCancel}
          aria-label={cancelLabel}
          className="icon-btn-sm flex-shrink-0"
        >
          <X size={20} aria-hidden="true" />
        </button>
      </div>
      <div className="px-4 pb-3 pt-1 bg-surface">
        <textarea
          ref={textareaRef}
          data-testid={inputTestId}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onFocus={() => setFocused(true)}
          onBlur={() => setFocused(false)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              onSave();
            }
          }}
          disabled={isSaving}
          rows={2}
          className={`chat-input-textarea w-full font-mono text-sm resize-none transition-colors rounded-control border-0 outline-none px-2 py-1 disabled:opacity-50 ${
            focused ? "is-focused bg-accent text-bg" : "bg-hover text-fg"
          }`}
        />
        <p className="text-2xs font-mono mt-1 text-muted">{hint}</p>
      </div>
    </div>
  );
};
