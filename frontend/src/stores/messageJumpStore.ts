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
   * Consume a pending jump for `conversationId`, if there is one.
   *
   * Returns the message id and moves it to `highlightedMessageId`, so the jump
   * fires exactly once per request and cannot re-trigger on every re-render.
   */
  claim(conversationId: string): string | null {
    if (!this.messageId || this.conversationId !== conversationId) {
      return null;
    }
    const messageId = this.messageId;
    this.conversationId = null;
    this.messageId = null;
    this.highlightedMessageId = messageId;
    return messageId;
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
