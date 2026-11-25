import React, { useEffect, useRef, useState } from "react";
import { ComposerBarHeader } from "./ComposerBarHeader";
import { isImeKey } from "../../utils/imeKey";

interface EditMessageBarProps {
  heading: string;
  cancelLabel: string;
  hint: string;
  value: string;
  onChange: (value: string) => void;
  /** Enter (without Shift) saves; Escape is the caller's. */
  onSave: () => void;
  onCancel: () => void;
  isSaving: boolean;
  testId: string;
  inputTestId: string;
  cancelTestId: string;
  /** Above the field: the edited text's link preview (#1987). */
  preview?: React.ReactNode;
}

/** Replaces the composer while editing; key it by message id. */
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
  preview,
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
      <ComposerBarHeader
        heading={heading}
        onCancel={onCancel}
        cancelLabel={cancelLabel}
        cancelTestId={cancelTestId}
      />
      {preview ? <div className="bg-surface">{preview}</div> : null}
      <div className="px-4 pb-3 pt-1 bg-surface">
        <textarea
          ref={textareaRef}
          data-testid={inputTestId}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onFocus={() => setFocused(true)}
          onBlur={() => setFocused(false)}
          onKeyDown={(e) => {
            // #2099: an IME's committing Enter is not a save.
            if (e.key === "Enter" && !e.shiftKey && !isImeKey(e)) {
              e.preventDefault();
              onSave();
            }
          }}
          disabled={isSaving}
          rows={2}
          className={`chat-input-textarea w-full font-mono text-body resize-none transition-colors rounded-control border-0 outline-none px-2 py-1 disabled:opacity-50 ${
            focused ? "is-focused bg-active text-fg" : "bg-hover text-fg"
          }`}
        />
        <p className="text-meta font-mono mt-1 text-muted">{hint}</p>
      </div>
    </div>
  );
};
