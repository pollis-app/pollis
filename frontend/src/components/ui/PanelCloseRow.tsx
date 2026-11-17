import React from "react";
import type { ActivateOn } from "../../hooks/usePressActivation";
import { PressButton } from "./PressButton";
import { Kbd } from "./Kbd";
import { FOCUS_RING } from "./focusRing";

interface PanelCloseRowProps {
  /** The visible word, e.g. "Close". */
  label: string;
  /** The toggle's shortcut, shown as a key chip. */
  shortcut: string;
  /** Names the action and its shortcut for assistive tech and the tooltip. */
  ariaLabel: string;
  onClick: () => void;
  "data-testid"?: string;
  /** Default `"press"`: closing a panel is free to undo (#1529). */
  activateOn?: ActivateOn;
}

/** The full-width footer row that closes a side panel, with its shortcut (#1967): the left sidebar's in Terminal, the right panel's in both looks. */
export const PanelCloseRow: React.FC<PanelCloseRowProps> = ({
  label,
  shortcut,
  ariaLabel,
  onClick,
  "data-testid": testId,
  activateOn = "press",
}) => (
  <PressButton
    data-testid={testId}
    activateOn={activateOn}
    onClick={onClick}
    aria-label={ariaLabel}
    title={ariaLabel}
    className={`flex shrink-0 items-center gap-2 px-2.5 min-h-composer-flush border-t border-line text-secondary text-start cursor-pointer transition-colors text-muted hover:text-fg ${FOCUS_RING}`}
  >
    <span className="flex-1">{label}</span>
    <Kbd>{shortcut}</Kbd>
  </PressButton>
);
