import React from "react";

interface MentionTokenProps {
  name: string;
  isSelf: boolean;
  skin: "terminal" | "refined";
}

/** A resolved `@mention` (#843): a filled block for you, a quiet chip for others. */
export const MentionToken: React.FC<MentionTokenProps> = ({ name, isSelf, skin }) => {
  const shared = "rounded-chip px-1";
  const font = skin === "terminal" ? "font-mono" : "";

  const tone = isSelf
    ? "bg-accent text-bg font-semibold"
    : "bg-active text-accent";

  // `<bdi>` isolates direction so "@" doesn't reorder in RTL text.
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
