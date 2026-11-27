import { senderColorKey, senderName } from "../../utils/senderName";
import React from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { X } from 'lucide-react';
import type { Message } from '../../types';
import { getUsernameColor, useBackgroundIsLight } from '../../utils/usernameColor';
import { PressButton } from "../ui/PressButton";
import { IconButton } from "../ui/IconButton";
import { useSelectedGroupAliases } from '../../hooks/queries/useGroupAliases';
import { aliasName } from '../../utils/groupAlias';

interface ReplyPreviewProps {
  messageId: string;
  allMessages: Message[];
  onDismiss: () => void;
  onScrollToMessage?: (messageId: string) => void;
}

export const ReplyPreview: React.FC<ReplyPreviewProps> = ({
  messageId,
  allMessages,
  onDismiss,
  onScrollToMessage,
}) => {
  const { t } = useTranslation('chat');
  const isLightBg = useBackgroundIsLight();
  const aliases = useSelectedGroupAliases();
  const message = allMessages.find((m) => m.id === messageId);
  if (!message) {
    return null;
  }
  const author = aliasName(aliases, message.sender_id, senderName(message));
  const authorColor = getUsernameColor(senderColorKey(message), isLightBg);
  // The text, else the first file's name, else (no text on this device) that it could not be decrypted.
  const content =
    message.content_decrypted ||
    (message.content_decrypted != null ? message.attachments?.[0]?.filename : undefined) ||
    t('message.undecryptable.title');
  const snippet = content.length > 80 ? content.substring(0, 80) + '...' : content;

  return (
    <div
      data-testid="reply-preview"
      className="flex items-center gap-2 px-4 py-1.5 flex-shrink-0 border-t border-line bg-surface"
    >
      <div className="flex-1 min-w-0">
        <span className="text-meta font-mono uppercase tracking-widest text-muted">
          <Trans
            t={t}
            i18nKey="replyBar.replyingTo"
            values={{ name: author }}
            components={{
              name: (
                <span className="font-semibold" style={{ color: authorColor }} />
              ),
            }}
          />
        </span>
        <PressButton
          data-testid="reply-preview-scroll-button"
          onClick={() => onScrollToMessage?.(messageId)}
          aria-label={t('replyBar.scrollToMessage')}
          className="block w-full text-start"
        >
          <p className="text-secondary font-mono truncate text-accent-dim">{snippet}</p>
        </PressButton>
      </div>
      <IconButton
        data-testid="dismiss-reply-button"
        onClick={onDismiss}
        aria-label={t('replyBar.dismiss')}
        className="flex-shrink-0"
      >
        <X size={17} aria-hidden="true" />
      </IconButton>
    </div>
  );
};
