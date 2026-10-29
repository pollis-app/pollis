import React from "react";

interface EmptyStateProps {
  children: React.ReactNode;
  testId?: string;
  messageTestId?: string;
  /** `dim` is louder; default `muted`. */
  tone?: "muted" | "dim";
  /** Paint the page background; off inside a container that already paints one. */
  background?: boolean;
  actions?: React.ReactNode;
}

/**
 * A centred "nothing to show here" line (#874). Not a loading state: those
 * render a spinner, and "still fetching" must not look like "nothing".
 */
export const EmptyState: React.FC<EmptyStateProps> = ({
  children,
  testId,
  messageTestId,
  tone = "muted",
  background = true,
  actions,
}) => {
  const wrapper = [
    "flex flex-1 items-center justify-center",
    actions ? "flex-col gap-3" : "",
    background ? "bg-bg" : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div data-testid={testId} className={wrapper}>
      <p
        data-testid={messageTestId}
        className={`text-xs font-mono ${tone === "dim" ? "text-dim" : "text-muted"}`}
      >
        {children}
      </p>
      {actions}
    </div>
  );
};
