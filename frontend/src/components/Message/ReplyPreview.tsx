import React from 'react';
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
  const isLightBg = useBackgroundIsLight();
  const message = allMessages.find((m) => m.id === messageId);
  if (!message) {
    return null;
  }
  const author = message.sender_username ?? message.sender_id;
  const authorColor = getUsernameColor(author, isLightBg);
  const content = message.content_decrypted || '[Encrypted message]';
  const snippet = content.length > 80 ? content.substring(0, 80) + '...' : content;

  return (
    <div
      data-testid="reply-preview"
      className="flex items-center gap-2 px-4 py-1.5 flex-shrink-0"
      style={{ borderTop: '1px solid var(--c-border)', background: 'var(--c-surface)' }}
    >
      <div className="flex-1 min-w-0">
        <span className="text-2xs font-mono uppercase tracking-widest" style={{ color: 'var(--c-text-muted)' }}>
          replying to{' '}
          <span className="font-semibold" style={{ color: authorColor }}>
            {author}
          </span>
        </span>
        <button
          data-testid="reply-preview-scroll-button"
          onClick={() => onScrollToMessage?.(messageId)}
          aria-label="Scroll to replied message"
          className="block w-full text-left"
        >
          <p className="text-xs font-mono truncate" style={{ color: 'var(--c-accent-dim)' }}>{snippet}</p>
        </button>
      </div>
      <button
        data-testid="dismiss-reply-button"
        onClick={onDismiss}
        aria-label="Dismiss reply"
        className="icon-btn-sm flex-shrink-0"
      >
        <X size={17} aria-hidden="true" />
      </button>
    </div>
  );
};
