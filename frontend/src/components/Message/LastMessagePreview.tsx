import React from "react";
import { useLastMessage } from "../../hooks/queries/useMessages";
import { ScrambleText } from "../ui/ScrambleText";

interface LastMessagePreviewProps {
  channelId?: string;
  conversationId?: string;
}

export const LastMessagePreview: React.FC<LastMessagePreviewProps> = ({ channelId, conversationId }) => {
  const { data: message, isLoading } = useLastMessage(channelId ?? null, conversationId ?? null);

  const text = message?.content_decrypted
    ? (message.sender_username
        ? `${message.sender_username}: ${message.content_decrypted}`
        : message.content_decrypted)
    : null;

  if (!isLoading && !text) {
    return null;
  }

  return (
    <ScrambleText
      text={text}
      placeholderLength={24}
      typeSpeed={25}
    />
  );
};
