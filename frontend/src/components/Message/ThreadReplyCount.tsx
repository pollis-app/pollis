import React from "react";
import { MessagesSquare } from "lucide-react";

interface ThreadReplyCountProps {
  /** Replies this device holds for this message's thread. */
  count: number;
  /** Omitted where threads aren't available, which also hides the affordance. */
  onOpen?: () => void;
}

/**
 * The "N replies" affordance under a message that has a thread (#825) — the
 * only discoverable entry point into a thread from the channel, mirroring how
 * Slack surfaces one.
 *
 * Renders nothing when there are no replies, so an ordinary message is
 * completely unchanged.
 */
export const ThreadReplyCount: React.FC<ThreadReplyCountProps> = ({
  count,
  onOpen,
}) => {
  if (count <= 0 || !onOpen) {
    return null;
  }

  return (
    <button
      type="button"
      onClick={onOpen}
      data-testid="thread-reply-count"
      className="mt-1 flex items-center gap-1 rounded px-1 py-0.5 text-xs text-accent hover:bg-hover"
    >
      <MessagesSquare size={12} aria-hidden />
      {count === 1 ? "1 reply" : `${count} replies`}
    </button>
  );
};
