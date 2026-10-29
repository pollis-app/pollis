// The message a pin, saved message or permalink asked to jump to (desktop's twin); claimed once by the chat screen.

import { makeAutoObservable } from "mobx";

class MessageJumpStore {
  conversationId: string | null = null;
  messageId: string | null = null;

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  request(conversationId: string, messageId: string) {
    this.conversationId = conversationId;
    this.messageId = messageId;
  }

  /** Fires exactly once per request. */
  claim(messageId: string): boolean {
    if (this.messageId !== messageId) {
      return false;
    }
    this.reset();
    return true;
  }

  reset() {
    this.conversationId = null;
    this.messageId = null;
  }
}

export const messageJumpStore = new MessageJumpStore();
