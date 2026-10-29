import { makeAutoObservable } from "mobx";

/**
 * The message a permalink asked us to jump to (#854). In MobX, not the URL:
 * the root route drops unknown params, and a message id is metadata that
 * shouldn't sit in navigation history. `MessageList` claims, scrolls, flashes.
 */
class MessageJumpStore {
  conversationId: string | null = null;
  messageId: string | null = null;
  /** Currently flashing, so the row can style itself. */
  highlightedMessageId: string | null = null;

  constructor() {
    makeAutoObservable(this);
  }

  request(conversationId: string, messageId: string) {
    this.conversationId = conversationId;
    this.messageId = messageId;
  }

  /**
   * Consume the pending jump. Keyed on the message the list RENDERS, since
   * `conversationId` is the MLS group id for channels. Moving it to
   * `highlightedMessageId` makes it fire once per request.
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

  clearHighlight() {
    this.highlightedMessageId = null;
  }

  /** Drop everything, e.g. when a permalink fails to resolve. */
  reset() {
    this.conversationId = null;
    this.messageId = null;
    this.highlightedMessageId = null;
  }
}

export const messageJumpStore = new MessageJumpStore();
