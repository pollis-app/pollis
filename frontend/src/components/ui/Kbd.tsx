import React from "react";

interface KbdProps {
  children: React.ReactNode;
}

/** A key or shortcut beside an action (#1967): machine face, the chip radius, a hairline edge on `bg`. Takes its parent's colour; decorative, since the action's own label names the shortcut. */
export const Kbd: React.FC<KbdProps> = ({ children }) => (
  <kbd
    aria-hidden="true"
    className="font-machine text-meta leading-[1.2] rounded-chip border border-line bg-bg px-1.5 py-px"
  >
    {children}
  </kbd>
);
