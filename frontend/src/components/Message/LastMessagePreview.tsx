import React from "react";
import { useTranslation } from "react-i18next";
import { ScrambleText } from "../ui/ScrambleText";
import type { Message } from "../../types";

interface LastMessagePreviewProps {
  // Handed down: the list owner batches every row's preview into one call (#874).
  message?: Message;
  isLoading?: boolean;
}

export const LastMessagePreview: React.FC<LastMessagePreviewProps> = ({ message, isLoading }) => {
  const { t } = useTranslation("chat");

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
    if (message.undecryptable) { return t("preview.undecryptable"); }
    const attCount = message.attachments?.length ?? 0;
    if (attCount > 0) {
      const summary = t("preview.attachments", { count: attCount });
      return message.sender_username
        ? t("preview.withSender", { name: message.sender_username, text: summary })
        : summary;
    }
    return t("preview.noMessages");
  })();

    // While the batch loads, a scrambling placeholder keeps the row height.
  if (isLoading && !text) {
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
