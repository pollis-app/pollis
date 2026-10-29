import React from "react";

interface MentionTokenProps {
  name: string;
  /** `@you` or `@all`. */
  isSelf: boolean;
  skin: "terminal" | "refined";
}

/**
 * A resolved `@mention` (#843). A mention of YOU is a filled accent block; of
 * someone else, a quiet tinted chip. Terminal uses its inverted-block idiom,
 * refined a rounded chip.
 */
export const MentionToken: React.FC<MentionTokenProps> = ({ name, isSelf, skin }) => {
  const shared = "rounded-chip px-1";
  const font = skin === "terminal" ? "font-mono" : "";

  const tone = isSelf
    ? "bg-accent text-bg font-semibold"
    : "bg-active text-accent";

  // `<bdi>`: without isolation a Latin handle in RTL text reorders the "@".
  return (
    <bdi
      data-testid={isSelf ? "mention-self" : "mention-other"}
      data-mention={name}
      className={`${shared} ${font} ${tone}`}
    >
      @{name}
    </bdi>
  );
};
