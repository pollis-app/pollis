import React from "react";
import { useTypingState } from "../hooks/useTypingState";

interface TypingIndicatorProps {
  channelId: string | null;
  conversationId: string | null;
}

function describe(usernames: string[]): string | null {
  if (usernames.length === 0) {
    return null;
  }
  if (usernames.length === 1) {
    return `${usernames[0]} is typing…`;
  }
  if (usernames.length === 2) {
    return `${usernames[0]} and ${usernames[1]} are typing…`;
  }
  return `${usernames.length} people are typing…`;
}

/** "X is typing…" above the input, in a fixed-height row. */
export const TypingIndicator: React.FC<TypingIndicatorProps> = ({
  channelId,
  conversationId,
}) => {
  const usernames = useTypingState({ channelId, conversationId });
  const text = describe(usernames);

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
