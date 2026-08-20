import React from "react";

interface InlineGhostProps {
  /** The textarea's full current value. Rendered invisibly to position the ghost. */
  value: string;
  /** The completion suffix shown after the caret, e.g. "na" for "@da" → "@dana". */
  ghost: string;
  /** Whether the textarea is focused — the terminal composer inverts its colours. */
  focused: boolean;
  /** The textarea's scrollTop, so the mirror stays aligned on a wrapped message. */
  scrollTop: number;
  /** Which completion track this ghost belongs to — mentions, or `:shortcodes:`. */
  testId: string;
}

/**
 * Terminal skin's inline autosuggest (#843) — the completion appears as
 * dimmed text directly after the caret and Tab accepts it. Shared by both
 * completion tracks: `@mention` and `:shortcode:` alike, since the only thing
 * that differs between them is the string being ghosted.
 *
 * This is a MIRROR, not a pop-over: an absolutely-positioned, pointer-events-
 * none copy of the textarea's box that renders the current value with
 * `visibility: hidden` (which preserves layout exactly) followed by the ghost.
 * The ghost therefore lands precisely where the caret is, with no measurement
 * and no floating element. Nothing here is fixed-position, portalled, or
 * overlaid on the app — it is a child of the composer's own input box.
 *
 * The caller only supplies a `ghost` while the caret sits at the END of the
 * value, which is exactly how fish and zsh-autosuggestions behave: the
 * suggestion is offered for the tail of the line, and typing in the middle of
 * a line simply doesn't offer one. That is the terminal idiom this skin is
 * imitating, not a limitation worked around.
 */
export const InlineGhost: React.FC<InlineGhostProps> = ({
  value,
  ghost,
  focused,
  scrollTop,
  testId,
}) => {
  if (!ghost) {
    return null;
  }

  return (
    <div
      className="absolute inset-0 overflow-hidden pointer-events-none"
      aria-hidden="true"
    >
      {/* Typography here MUST match the textarea in ChatInput exactly —
          same padding, font, size and line-height — or the ghost drifts. */}
      <div
        className="px-2 py-1 font-mono text-sm whitespace-pre-wrap break-words"
        style={{ lineHeight: "1.5rem", transform: `translateY(${-scrollTop}px)` }}
      >
        <span className="invisible">{value}</span>
        {/* Focused, the terminal input is an accent block with bg-coloured
            text, so the ghost dims the bg colour; unfocused it dims the
            normal foreground. */}
        <span
          data-testid={testId}
          className={focused ? "text-bg opacity-60" : "text-muted"}
        >
          {ghost}
        </span>
      </div>
    </div>
  );
};
