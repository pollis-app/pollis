import React from "react";
import { BotBadge } from "../ui/BotBadge";

interface MentionTokenProps {
  /** The `@handle` as written. */
  name: string;
  /** The handle's alias in this group (#2003), shown in place of the handle. */
  label?: string;
  isSelf: boolean;
  /** The handle is one of the group's bots (#1182): the Bot badge follows the token. */
  isBot?: boolean;
  /** The group, for the badge's "Managed by the admins of …". */
  groupName?: string;
  skin: "terminal" | "modern";
}

/** A resolved `@mention` (#843): a filled block for you, a quiet chip for others. */
export const MentionToken: React.FC<MentionTokenProps> = ({ name, label, isSelf, isBot = false, groupName, skin }) => {
  const shared = "rounded-chip px-1";
  const font = skin === "terminal" ? "font-mono" : "";

  const tone = isSelf
    ? "bg-accent text-bg font-semibold"
    : "bg-active text-accent";

  // `<bdi>` isolates direction so "@" doesn't reorder in RTL text.
  const token = (
    <bdi
      data-testid={isSelf ? "mention-self" : "mention-other"}
      data-mention={name}
      title={label ? `@${name}` : undefined}
      className={`${shared} ${font} ${tone}`}
    >
      @{label ?? name}
    </bdi>
  );
  if (!isBot) {
    return token;
  }
  return (
    <>
      {token}{" "}
      <BotBadge groupName={groupName} data-testid="mention-bot-badge" />
    </>
  );
};
