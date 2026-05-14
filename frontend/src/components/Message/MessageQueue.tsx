import React from 'react';
import { X, Clock, Send, AlertCircle } from 'lucide-react';
import { useAppStore } from '../../stores/appStore';
import { Button } from '../ui/Button';

export const MessageQueue: React.FC = () => {
  const {
    messageQueue,
    removeFromMessageQueue,
    updateMessageQueueItem,
  } = useAppStore();

  const pendingMessages = messageQueue.filter(
    (item) => item.status === 'pending' || item.status === 'sending'
  );

  const failedMessages = messageQueue.filter((item) => item.status === 'failed');

  if (pendingMessages.length === 0 && failedMessages.length === 0) {
    return null;
  }

  const getMessageContent = (_messageId: string): string => {
    return '[Pending message]';
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
      className="flex flex-col gap-1 px-4 py-2 flex-shrink-0"
      style={{ borderTop: '1px solid var(--c-border)', background: 'var(--c-surface)' }}
    >
      <span className="text-2xs font-mono uppercase tracking-widest" style={{ color: 'var(--c-text-muted)' }}>
        Queue
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
                <Send size={14} aria-hidden="true" style={{ color: 'var(--c-accent)' }} />
              ) : (
                <Clock size={14} aria-hidden="true" style={{ color: 'var(--c-text-muted)' }} />
              )}
              <span
                data-testid="queue-item-status"
                className="text-2xs font-mono"
                style={{ color: 'var(--c-text-muted)' }}
              >
                {item.status}
              </span>
              <p className="text-xs font-mono flex-1 truncate" style={{ color: 'var(--c-text-dim)' }}>{snippet}</p>
              <button
                data-testid={`cancel-queue-item-${item.id}`}
                onClick={() => handleCancel(item.id, item.message_id)}
                aria-label="Cancel message"
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
              <AlertCircle size={14} aria-hidden="true" style={{ color: '#ff6b6b' }} />
              <span className="text-2xs font-mono" style={{ color: '#ff6b6b' }}>
                failed ×{item.retry_count}
              </span>
              <p className="text-xs font-mono flex-1 truncate" style={{ color: 'var(--c-text-dim)' }}>{snippet}</p>
              <div className="flex items-center gap-1">
                <Button
                  data-testid={`retry-queue-item-${item.id}`}
                  onClick={() => handleRetry(item.id)}
                  variant="ghost"
                  size="xs"
                >
                  retry
                </Button>
                <button
                  data-testid={`cancel-queue-item-${item.id}`}
                  onClick={() => handleCancel(item.id, item.message_id)}
                  aria-label="Cancel message"
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
};
