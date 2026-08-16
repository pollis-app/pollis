import React from "react";

interface EmptyStateProps {
  /** The line itself — "no messages yet", "sign in to continue", … */
  children: React.ReactNode;
  /** Optional testid on the wrapper. */
  testId?: string;
  /** Optional testid on the message paragraph, where a spec asserts on it. */
  messageTestId?: string;
  /**
   * `dim` is the slightly louder of the two. Default `muted`, which is what
   * every "there is nothing here" line uses.
   */
  tone?: "muted" | "dim";
  /**
   * Paint the page background. Off for a state drawn inside a container that
   * already paints one, where an extra `bg-bg` would show through as a patch.
   */
  background?: boolean;
  /** A recovery affordance under the line, e.g. "go home". */
  actions?: React.ReactNode;
}

/**
 * A centred "there is nothing to show here" line filling the space it is
 * given.
 *
 * Hand-rolled a dozen times before this existed — same flex centring, same
 * `text-xs font-mono`, same muted colour, copied from page to page and drifting
 * in exactly the ways copies drift (some painted a background, some did not;
 * some spelled the colour as an inline `var(--c-text-muted)` and some as the
 * `text-muted` utility) (#874).
 *
 * Deliberately NOT a loading state: those render a spinner, and conflating
 * "still fetching" with "there is nothing" is a real bug this component must
 * not make easy. `NavigableList` is the exception, and passes a label for a
 * loading line that has always been text rather than a spinner.
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
