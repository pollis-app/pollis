import React from "react";
import { useTranslation } from "react-i18next";
import { Button } from "./Button";
import type { ActivateOn } from "../../hooks/usePressActivation";

export interface FormCancel {
  onClick: () => void;
  /** Default "Cancel". */
  label?: string;
  testId?: string;
  disabled?: boolean;
  activateOn?: ActivateOn;
  /** Takes focus on mount: a confirmation's default is the way out, never the action. */
  autoFocus?: boolean;
}

interface FormActionsProps {
  /** The page's action(s), the one that matters first: `primary`, or `danger` when it destroys. */
  children?: React.ReactNode;
  cancel?: FormCancel;
}

/**
 * The one actions row (#1968): left-aligned under the fields, buttons at their natural width,
 * the action first and a ghost Cancel last.
 */
export const FormActions: React.FC<FormActionsProps> = ({ children, cancel }) => {
  const { t } = useTranslation("common");
  return (
    <div className="flex flex-wrap items-center gap-2">
      {children}
      {cancel && (
        <Button
          variant="ghost"
          data-testid={cancel.testId}
          disabled={cancel.disabled}
          activateOn={cancel.activateOn}
          autoFocus={cancel.autoFocus}
          onClick={cancel.onClick}
        >
          {cancel.label ?? t("actions.cancel")}
        </Button>
      )}
    </div>
  );
};
