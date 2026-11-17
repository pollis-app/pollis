import React from "react";
import { X } from "lucide-react";
import { IconButton } from "../ui/IconButton";

interface StatusBarDismissButtonProps {
  ariaLabel: string;
  onClick: () => void;
  "data-testid": string;
}

/** The X that clears a status-bar notice (#1967): icon-sized to fit the bar's one-line slot, in the bar's own ink. */
export const StatusBarDismissButton: React.FC<StatusBarDismissButtonProps> = ({ ariaLabel, onClick, "data-testid": testId }) => (
  <IconButton
    data-testid={testId}
    onClick={onClick}
    aria-label={ariaLabel}
    className="h-4 w-4 text-inherit hover:text-inherit"
  >
    <X className="w-4 h-4" />
  </IconButton>
);
