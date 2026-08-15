import React from "react";

interface MentionTokenProps {
  /** Username as typed, without the leading '@'. */
  name: string;
  /** True when this mention addresses the reader — `@you` or `@all`. */
  isSelf: boolean;
  skin: "terminal" | "refined";
}

/**
 * A resolved `@mention` inside a message body (#843).
 *
 * Two strengths, deliberately far apart: a mention of YOU is a filled accent
 * block you cannot scroll past, while a mention of someone else is a quiet
 * tinted chip that reads as a name rather than an alert. That difference is
 * the whole point — a mention should feel like being spoken to.
 *
 * Each skin renders the same distinction in its own idiom: terminal keeps the
 * square, inverted-block treatment it already uses for admin name badges;
 * refined uses the rounded chip Slack made familiar.
 */
export const MentionToken: React.FC<MentionTokenProps> = ({ name, isSelf, skin }) => {
  const shared = "rounded-[var(--radius-chip)] px-1";
  const font = skin === "terminal" ? "font-mono" : "";

  const tone = isSelf
    ? "bg-accent text-bg font-semibold"
    : "bg-active text-accent";

  return (
    <span
      data-testid={isSelf ? "mention-self" : "mention-other"}
      data-mention={name}
      className={`${shared} ${font} ${tone}`}
    >
      @{name}
    </span>
  );
};
