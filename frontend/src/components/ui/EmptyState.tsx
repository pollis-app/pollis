import React from "react";

interface EmptyStateProps {
  /** One plain sentence saying what is missing. */
  children: React.ReactNode;
  testId?: string;
  messageTestId?: string;
  /** Default `muted`. */
  tone?: "muted" | "dim";
  /** Off inside a container that already paints one. */
  background?: boolean;
  /** What goes under the sentence: the action that fills the space, when there is one. */
  actions?: React.ReactNode;
  /** It replaced a loading state: fade in (#2125). */
  fadeIn?: boolean;
}

/**
 * The one "nothing here" treatment for every empty list (#874, #1969): a centred
 * sentence, and the action that fills the space when there is one. Not a
 * loading state. Mobile's twin is `EmptyState` in `mobile/components/ui.tsx`.
 */
export const EmptyState: React.FC<EmptyStateProps> = ({
  children,
  testId,
  messageTestId,
  tone = "muted",
  background = true,
  actions,
  fadeIn = false,
}) => {
  const wrapper = [
    "flex flex-1 items-center justify-center px-4 py-8 text-center",
    actions ? "flex-col gap-3" : "",
    background ? "bg-bg" : "",
    fadeIn ? "motion-fade" : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div data-testid={testId} data-empty-state="" className={wrapper}>
      <p
        data-testid={messageTestId}
        className={`text-secondary font-mono ${tone === "dim" ? "text-dim" : "text-muted"}`}
      >
        {children}
      </p>
      {actions}
    </div>
  );
};
