import React from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { useTypingState } from "../hooks/useTypingState";

interface TypingIndicatorProps {
  channelId: string | null;
  conversationId: string | null;
}

function describe(usernames: string[], t: TFunction): string | null {
  if (usernames.length === 0) {
    return null;
  }
  if (usernames.length === 1) {
    return t("chat:typing.one", { name: usernames[0] });
  }
  if (usernames.length === 2) {
    return t("chat:typing.two", { first: usernames[0], second: usernames[1] });
  }
  return t("chat:typing.several", { count: usernames.length });
}

/** "X is typing…" above the input, in a fixed-height row. */
export const TypingIndicator: React.FC<TypingIndicatorProps> = ({
  channelId,
  conversationId,
}) => {
  const usernames = useTypingState({ channelId, conversationId });
  const { t } = useTranslation();
  const text = describe(usernames, t);

  return (
    <div
      data-testid="typing-indicator"
      className="px-4 text-xs font-mono text-muted"
      style={{
        minHeight: 16,
        lineHeight: "16px",
        margin: "0.25rem 0.125rem"
      }}
    >
      {text}
    </div>
  );
};
