import React from "react";
import { useTranslation } from "react-i18next";
import { ScrambleText } from "../ui/ScrambleText";
import type { Message } from "../../types";
import { useSkin } from "../../hooks/queries/usePreferences";
import { aliasName, type AliasMap } from "../../utils/groupAlias";

interface LastMessagePreviewProps {
  // Handed down: the list batches every row's preview in one call (#874).
  message?: Message;
  isLoading?: boolean;
  /** A channel's group aliases (#2003): the sender is named as the group shows them. */
  aliases?: AliasMap;
}

export const LastMessagePreview: React.FC<LastMessagePreviewProps> = ({ message, isLoading, aliases }) => {
  const { t } = useTranslation("chat");
  // Bracketed forms are Terminal's (#1965).
  const isTerminal = useSkin() === "terminal";
  const sender = message ? aliasName(aliases, message.sender_id, message.sender_display_name || message.sender_username) : undefined;

  const text = message?.content_decrypted
    ? (sender
        ? t("preview.withSender", {
            name: sender,
            text: message.content_decrypted,
          })
        : message.content_decrypted)
    : null;

  const fallbackText = (() => {
    if (!message) { return t("preview.noMessages"); }
    if (message.undecryptable) { return t("preview.undecryptable"); }
    const attCount = message.attachments?.length ?? 0;
    if (attCount > 0) {
      const summary = t(isTerminal ? "preview.attachmentsTerminal" : "preview.attachments", { count: attCount });
      return sender
        ? t("preview.withSender", { name: sender, text: summary })
        : summary;
    }
    return t("preview.noMessages");
  })();

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
