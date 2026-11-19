import React from "react";

interface PageColumnProps {
  children: React.ReactNode;
  /** `form` when the column is the form itself, so Enter in a field submits it. */
  as?: "div" | "form";
  /** `fields` stacks a form's controls; `sections` spaces a settings page's sections. */
  spacing?: "fields" | "sections";
  /** On the scrolling pane around the column. */
  paneTestId?: string;
  "data-testid"?: string;
  onSubmit?: (e: React.FormEvent<HTMLFormElement>) => void;
  /** Type only (a page's face or ink); never width, gap or padding. */
  className?: string;
}

const GAP = { fields: "gap-5", sections: "gap-8" } as const;

/**
 * The one page column on desktop (#1968): every form, settings and confirmation page is a single
 * column, at most 420px (`max-w-md` at the 15px root), top-aligned and centred in the main pane,
 * with no card around it. The pane scrolls; the column never does.
 */
export const PageColumn: React.FC<PageColumnProps> = ({
  children,
  as = "div",
  spacing = "fields",
  paneTestId,
  "data-testid": testId,
  onSubmit,
  className = "",
}) => {
  const columnClass = `flex w-full max-w-md flex-col ${GAP[spacing]} ${className}`;
  return (
    <div
      data-testid={paneTestId}
      className="flex min-h-0 flex-1 justify-center overflow-auto bg-bg px-6 py-8"
    >
      {as === "form" ? (
        <form data-testid={testId} onSubmit={onSubmit} className={columnClass}>
          {children}
        </form>
      ) : (
        <div data-testid={testId} className={columnClass}>
          {children}
        </div>
      )}
    </div>
  );
};
