import { makeAutoObservable } from "mobx";

/** The permalink jump target (#854); MobX, not the URL, so the id stays out of history. */
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

  request(conversationId: string, messageId: string) {
    this.conversationId = conversationId;
    this.messageId = messageId;
  }

  /** Consume the jump if this message is rendered; moves it to `highlightedMessageId` so it fires once. */
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
