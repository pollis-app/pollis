import React from "react";
import { useLastMessage } from "../../hooks/queries/useMessages";
import { ScrambleText } from "../ui/ScrambleText";

interface LastMessagePreviewProps {
  channelId?: string;
  conversationId?: string;
}

export const LastMessagePreview: React.FC<LastMessagePreviewProps> = ({ channelId, conversationId }) => {
  const { data: message, isLoading, isFetching } = useLastMessage(channelId ?? null, conversationId ?? null);

  const text = message?.content_decrypted
    ? (message.sender_username
        ? `${message.sender_username}: ${message.content_decrypted}`
        : message.content_decrypted)
    : null;

  // While initial load or refetch with no prior data, show a scrambling placeholder
  // so the row height never collapses.
  if (isLoading || (isFetching && !text)) {
    return <ScrambleText text={null} placeholderLength={24} typeSpeed={25} />;
  }

  return (
    <ScrambleText
      text={text ?? "No messages"}
      placeholderLength={24}
      typeSpeed={25}
    />
  );
};
