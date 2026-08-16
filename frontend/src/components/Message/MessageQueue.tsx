import React from 'react';
import { useTranslation } from 'react-i18next';
import { X, Clock, Send, AlertCircle } from 'lucide-react';
import { observer } from 'mobx-react-lite';
import { appStore } from '../../stores/appStore';
import { Button } from '../ui/Button';

export const MessageQueue: React.FC = observer(() => {
  const { t } = useTranslation('chat');
  const {
    messageQueue,
    removeFromMessageQueue,
    updateMessageQueueItem,
  } = appStore;

  const pendingMessages = messageQueue.filter(
    (item) => item.status === 'pending' || item.status === 'sending'
  );

  const failedMessages = messageQueue.filter((item) => item.status === 'failed');

  if (pendingMessages.length === 0 && failedMessages.length === 0) {
    return null;
  }

  const getMessageContent = (_messageId: string): string => {
    return t('queue.pendingPlaceholder');
  };

  const handleCancel = (queueItemId: string, _messageId: string) => {
    updateMessageQueueItem(queueItemId, { status: 'cancelled' });
    removeFromMessageQueue(queueItemId);
  };

  const handleRetry = (queueItemId: string) => {
    updateMessageQueueItem(queueItemId, {
      status: 'pending',
      retry_count: messageQueue.find((q) => q.id === queueItemId)?.retry_count || 0,
    });
  };

  return (
    <div
      data-testid="message-queue"
      className="flex flex-col gap-1 px-4 py-2 flex-shrink-0 border-t border-line bg-surface"
    >
      <span className="text-2xs font-mono uppercase tracking-widest text-muted">
        {t('queue.heading')}
      </span>

      <div className="flex flex-col gap-1">
        {pendingMessages.map((item) => {
          const content = getMessageContent(item.message_id);
          const snippet = content.length > 60 ? content.substring(0, 60) + '...' : content;

          return (
            <div
              key={item.id}
              data-testid={`queue-item-${item.id}`}
              className="flex items-center gap-2"
            >
              {item.status === 'sending' ? (
                <Send size={14} aria-hidden="true" className="text-accent" />
              ) : (
                <Clock size={14} aria-hidden="true" className="text-muted" />
              )}
              <span
                data-testid="queue-item-status"
                className="text-2xs font-mono text-muted"
              >
                {t(`status.${item.status}`)}
              </span>
              <p className="text-xs font-mono flex-1 truncate text-dim">{snippet}</p>
              <button
                data-testid={`cancel-queue-item-${item.id}`}
                onClick={() => handleCancel(item.id, item.message_id)}
                aria-label={t('queue.cancel')}
                className="icon-btn-sm"
              >
                <X size={14} aria-hidden="true" />
              </button>
            </div>
          );
        })}

        {failedMessages.map((item) => {
          const content = getMessageContent(item.message_id);
          const snippet = content.length > 60 ? content.substring(0, 60) + '...' : content;

          return (
            <div
              key={item.id}
              data-testid={`queue-item-failed-${item.id}`}
              className="flex items-center gap-2"
            >
              <AlertCircle size={14} aria-hidden="true" className="text-danger" />
              <span className="text-2xs font-mono text-danger">
                {t('queue.failedCount', { count: item.retry_count })}
              </span>
              <p className="text-xs font-mono flex-1 truncate text-dim">{snippet}</p>
              <div className="flex items-center gap-1">
                <Button
                  data-testid={`retry-queue-item-${item.id}`}
                  onClick={() => handleRetry(item.id)}
                  variant="ghost"
                  size="xs"
                >
                  {t('queue.retry')}
                </Button>
                <button
                  data-testid={`cancel-queue-item-${item.id}`}
                  onClick={() => handleCancel(item.id, item.message_id)}
                  aria-label={t('queue.cancel')}
                  className="icon-btn-sm"
                >
                  <X size={14} aria-hidden="true" />
                </button>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
});
