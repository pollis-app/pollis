import React from "react";
import { FOCUS_RING } from "./focusRing";

interface LinkButtonProps {
  /** Omitted when `Trans` fills it in from the translation's `<tag>…</tag>`. */
  children?: React.ReactNode;
  onClick: () => void;
  /** `accent` in running text; `muted` for a quiet "learn more" under a status. Default `accent`. */
  tone?: "accent" | "muted";
  /** `inherit` sits in a sentence at its size; `meta` stands on its own line. Default `inherit`. */
  size?: "inherit" | "meta";
  /** A runtime colour (a status line's own colour), which wins over `tone`. */
  color?: string;
  "data-testid"?: string;
}

/** An action that reads as a link (#1967): underlined text in a sentence, or a "learn more" line. Opens or navigates; never a form's own action (use `Button`). */
export const LinkButton: React.FC<LinkButtonProps> = ({
  children,
  onClick,
  tone = "accent",
  size = "inherit",
  color,
  "data-testid": testId,
}) => (
  <button
    type="button"
    data-testid={testId}
    onClick={onClick}
    className={`inline p-0 border-0 bg-transparent underline cursor-pointer rounded-chip ${
      size === "meta" ? "text-meta self-start" : ""
    } ${tone === "muted" ? "text-muted hover:text-fg" : "text-accent"} ${FOCUS_RING}`}
    style={color ? { color } : undefined}
  >
    {children}
  </button>
);
