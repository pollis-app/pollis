import React from "react";

interface AppMarkProps {
  label: string;
  "data-testid"?: string;
}

/**
 * A mark on a bot's App (#2005): "Development build", "Can't send data outside this group".
 * The Bot badge's quiet tag (chip radius, `surfaceHigh`, `textDim`), never the accent; the
 * words carry the meaning, so it reads the same in every skin.
 */
export const AppMark: React.FC<AppMarkProps> = ({ label, "data-testid": testId }) => (
  <span
    data-testid={testId}
    className="flex-shrink-0 self-center rounded-chip bg-surface-high px-1.5 text-meta font-medium text-dim select-none"
  >
    {label}
  </span>
);
