import React from "react";
import { ChevronRight } from "lucide-react";
import { usePressActivation } from "../../hooks/usePressActivation";

interface NavRowProps {
  /** The row's visible text, e.g. a setting's current value. */
  children: React.ReactNode;
  onClick: () => void;
  /** BCP-47 tag of the text's own language, when it is not the UI's (an endonym). */
  lang?: string;
  /** Ids of the elements naming the row, e.g. a section heading plus the value. */
  "aria-labelledby"?: string;
  "data-testid"?: string;
  /** Id on the text span, so `aria-labelledby` can include it. */
  textId?: string;
}

/** A filled, unbordered row that opens another page; the chevron marks it as navigation. Acts on pointer-down (#1529). */
export const NavRow: React.FC<NavRowProps> = ({
  children,
  onClick,
  lang,
  "aria-labelledby": labelledBy,
  "data-testid": testId,
  textId,
}) => {
  const press = usePressActivation<HTMLButtonElement>(() => onClick(), "press");
  return (
    <button
      type="button"
      {...press}
      data-testid={testId}
      aria-labelledby={labelledBy}
      className="flex items-center gap-3 w-full text-start rounded-lg px-4 py-3 border-0 font-mono bg-surface-raised text-fg hover:bg-hover cursor-pointer focus:outline-none focus-visible:ring-4 focus-visible:ring-accent"
    >
      <span id={textId} className="flex-1 min-w-0 truncate text-sm" lang={lang}>
        {children}
      </span>
      <ChevronRight size={16} aria-hidden="true" className="shrink-0 text-muted rtl-mirror" />
    </button>
  );
};
