import React from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { X } from 'lucide-react';
import type { Message } from '../../types';
import { getUsernameColor, useBackgroundIsLight } from '../../utils/usernameColor';

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
  const message = allMessages.find((m) => m.id === messageId);
  if (!message) {
    return null;
  }
  const author = message.sender_username ?? message.sender_id;
  const authorColor = getUsernameColor(author, isLightBg);
  const content = message.content_decrypted || t('replyBar.encrypted');
  const snippet = content.length > 80 ? content.substring(0, 80) + '...' : content;

  return (
    <div
      data-testid="reply-preview"
      className="flex items-center gap-2 px-4 py-1.5 flex-shrink-0 border-t border-line bg-surface"
    >
      <div className="flex-1 min-w-0">
        <span className="text-2xs font-mono uppercase tracking-widest text-muted">
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
        <button
          data-testid="reply-preview-scroll-button"
          onClick={() => onScrollToMessage?.(messageId)}
          aria-label={t('replyBar.scrollToMessage')}
          className="block w-full text-start"
        >
          <p className="text-xs font-mono truncate text-accent-dim">{snippet}</p>
        </button>
      </div>
      <button
        data-testid="dismiss-reply-button"
        onClick={onDismiss}
        aria-label={t('replyBar.dismiss')}
        className="icon-btn-sm flex-shrink-0"
      >
        <X size={17} aria-hidden="true" />
      </button>
    </div>
  );
};
