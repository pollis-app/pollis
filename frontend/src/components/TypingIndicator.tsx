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

/**
 * Renders the "X is typing…" hint above the chat input. Reserves a stable
 * row (height-fixed via min-height) so the input doesn't visually jump as
 * indicators come and go.
 */
export const TypingIndicator: React.FC<TypingIndicatorProps> = ({
  channelId,
  conversationId,
}) => {
  const usernames = useTypingState({ channelId, conversationId });
  const text = describe(usernames);

  return (
    <div
      data-testid="typing-indicator"
      className="px-4 text-xs font-mono"
      style={{
        minHeight: 16,
        color: "var(--c-text-muted)",
        lineHeight: "16px",
        margin: "0.25rem 0.125rem"
      }}
    >
      {text}
    </div>
  );
};
