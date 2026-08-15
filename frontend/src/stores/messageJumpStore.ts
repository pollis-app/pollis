import { makeAutoObservable } from "mobx";

/**
 * The message a permalink asked us to jump to (#854).
 *
 * Ephemeral UI state, so it lives in MobX rather than the URL. Two reasons:
 *
 *  1. The root route's `validatePanelSearch` deliberately drops every search
 *     param it does not know, so a `?m=` would not survive navigation anyway.
 *  2. A message id is metadata. Keeping it out of the URL keeps it out of
 *     navigation history, which is the right default for a pointer at someone's
 *     private conversation.
 *
 * The flow is: resolve the permalink locally → set the target → navigate →
 * `MessageList` notices a target for the conversation it is rendering, scrolls
 * to it, flashes the highlight, and clears it.
 */
class MessageJumpStore {
  /** Conversation the pending jump belongs to. */
  conversationId: string | null = null;
  /** Message to scroll to and flash. */
  messageId: string | null = null;
  /** Message currently flashing, so the row can style itself. */
  highlightedMessageId: string | null = null;

  constructor() {
    makeAutoObservable(this);
  }

  /** Queue a jump. Called after a permalink resolves successfully. */
  request(conversationId: string, messageId: string) {
    this.conversationId = conversationId;
    this.messageId = messageId;
  }

  /**
   * Consume the pending jump, which the caller has confirmed it can satisfy.
   *
   * Claiming is keyed on the message the list actually RENDERS, not on a
   * conversation id: `MessageList`'s `conversationId` prop is the MLS *group*
   * id for channels (it drives roster banners), so comparing against it would
   * never match a channel permalink. "Do I show this message?" is both the
   * correct question and the one that cannot drift.
   *
   * Moves the id to `highlightedMessageId` so the jump fires exactly once per
   * request and cannot re-trigger on every re-render.
   */
  claimMessage(messageId: string): boolean {
    if (this.messageId !== messageId) {
      return false;
    }
    this.conversationId = null;
    this.messageId = null;
    this.highlightedMessageId = messageId;
    return true;
  }

  /** End the flash. */
  clearHighlight() {
    this.highlightedMessageId = null;
  }

  /** Drop everything — used when a permalink fails to resolve. */
  reset() {
    this.conversationId = null;
    this.messageId = null;
    this.highlightedMessageId = null;
  }
}

export const messageJumpStore = new MessageJumpStore();
