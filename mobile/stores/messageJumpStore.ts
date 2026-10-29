// The message a pin, saved message or permalink asked to jump to (desktop's
// twin). In MobX, not route params: a back carries no params, and on iPad the
// conversation is a pane, not a route. The chat screen pages back to the row
// (`lib/messageJump.ts`) and `claim`s it so it fires once.

import { makeAutoObservable } from "mobx";

class MessageJumpStore {
  /** Conversation (channel or DM id) the pending jump belongs to. */
  conversationId: string | null = null;
  /** Message to scroll to and tint. */
  messageId: string | null = null;

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  request(conversationId: string, messageId: string) {
    this.conversationId = conversationId;
    this.messageId = messageId;
  }

  /** Consume the pending jump: it fires exactly once per request. */
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
