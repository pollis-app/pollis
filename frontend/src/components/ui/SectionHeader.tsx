import React from "react";
import { useSkin } from "../../hooks/queries/usePreferences";

interface SectionHeaderProps {
  children: React.ReactNode;
  /** Hairline above it too, for every section after the first (Terminal's bar). */
  bordered?: boolean;
  "data-testid"?: string;
}

/**
 * A panel section's title (#1964): Members, Pinned, Safety number. Terminal draws the left
 * sidebar's sticky bar in small spaced capitals; Modern a sentence-case label in `textMuted`
 * (docs/design/README.md: sentence case everywhere in Modern). Children may lead with an icon
 * or a back button; they sit in a row with a small gap.
 */
export const SectionHeader: React.FC<SectionHeaderProps> = ({ children, bordered, "data-testid": testId }) => {
  const isTerminal = useSkin() === "terminal";
  const cls = isTerminal
    ? [
        "sticky top-0 z-[1] flex h-bar w-full items-center gap-2 border-b border-line bg-surface px-2.5",
        "text-[0.8rem] uppercase tracking-[0.08em] text-muted select-none",
        bordered ? "mt-1 border-t" : "",
      ]
        .filter(Boolean)
        .join(" ")
    : "flex items-center gap-2 px-2 pt-3 pb-1 text-secondary font-medium text-muted";
  return (
    <h2 className={cls} data-testid={testId}>
      {children}
    </h2>
  );
};
