import React from "react";
import { useTranslation } from "react-i18next";
import { useLastMessage } from "../../hooks/queries/useMessages";
import { ScrambleText } from "../ui/ScrambleText";

interface LastMessagePreviewProps {
  channelId?: string;
  conversationId?: string;
}

export const LastMessagePreview: React.FC<LastMessagePreviewProps> = ({ channelId, conversationId }) => {
  const { t } = useTranslation("chat");
  const { data: message, isLoading, isFetching } = useLastMessage(channelId ?? null, conversationId ?? null);

  const text = message?.content_decrypted
    ? (message.sender_username
        ? t("preview.withSender", {
            name: message.sender_username,
            text: message.content_decrypted,
          })
        : message.content_decrypted)
    : null;

  const fallbackText = (() => {
    if (!message) { return t("preview.noMessages"); }
    const attCount = message.attachments?.length ?? 0;
    if (attCount > 0) {
      const summary = t("preview.attachments", { count: attCount });
      return message.sender_username
        ? t("preview.withSender", { name: message.sender_username, text: summary })
        : summary;
    }
    return t("preview.noMessages");
  })();

  // While initial load or refetch with no prior data, show a scrambling placeholder
  // so the row height never collapses.
  if (isLoading || (isFetching && !text)) {
    return <ScrambleText text={null} placeholderLength={24} typeSpeed={25} />;
  }

  return (
    <ScrambleText
      text={text ?? fallbackText}
      placeholderLength={24}
      typeSpeed={25}
    />
  );
};
