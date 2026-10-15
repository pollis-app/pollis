// The message a pin, a saved message or a permalink asked to jump to — the
// mobile twin of desktop's `frontend/src/stores/messageJumpStore.ts`, same
// shape and same rules.
//
// Ephemeral UI state, so it lives in MobX rather than in route params: the
// pinned list returns to the conversation it was opened from (a back, which
// carries no params), and on iPad the conversation is a pane, not a route.
//
// The flow: resolve the permalink locally → `request` → navigate → the chat
// screen notices a target for the conversation it shows, pages back until
// the row is rendered (`lib/messageJump.ts`), scrolls to it, tints it, and
// `claim`s it so it fires once.

import { makeAutoObservable } from "mobx";

class MessageJumpStore {
  /** Conversation (channel or DM id) the pending jump belongs to. */
  conversationId: string | null = null;
  /** Message to scroll to and tint. */
  messageId: string | null = null;

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  /** Queue a jump. Called after a permalink resolves. */
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

  /** Drop it — the target cannot be reached, or the session ended. */
  reset() {
    this.conversationId = null;
    this.messageId = null;
  }
}

export const messageJumpStore = new MessageJumpStore();
