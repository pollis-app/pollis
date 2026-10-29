import React from "react";

interface EmptyStateProps {
  children: React.ReactNode;
  testId?: string;
  messageTestId?: string;
  /** Default `muted`. */
  tone?: "muted" | "dim";
  /** Off inside a container that already paints one. */
  background?: boolean;
  actions?: React.ReactNode;
}

/** A centred "nothing here" line (#874). Not a loading state. */
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
