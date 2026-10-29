import { makeAutoObservable } from "mobx";
import {
  transitionMessageNav,
  type MessageNavAction,
  type MessageNavEvent,
  type MessageNavState,
} from "../utils/messageNav";

/**
 * What the ONE navigable `MessageList` (the main chat) lends the navigator
 * while mounted; callbacks read live component state.
 */
export interface MessageNavContext {
  orderedIds: () => string[];
  /** One message's action bar, in visual order, from the rendered DOM. */
  actionsFor: (messageId: string) => MessageNavAction[];
  focusComposer: () => void;
}

/**
 * Live keyboard-navigation state for the chat log (machine and invariants in
 * `utils/messageNav.ts`); all mutation goes through `dispatch`.
 */
class MessageNavStore {
  state: MessageNavState = { mode: "composer" };
  private ctx: MessageNavContext | null = null;

  constructor() {
    makeAutoObservable(this);
  }

  get active(): boolean {
    return this.state.mode !== "composer";
  }

  registerContext(ctx: MessageNavContext): void {
    this.ctx = ctx;
    this.state = { mode: "composer" };
  }

  unregisterContext(ctx: MessageNavContext): void {
    if (this.ctx === ctx) {
      this.ctx = null;
      this.state = { mode: "composer" };
    }
  }

  dispatch(event: MessageNavEvent): void {
    const ctx = this.ctx;
    if (!ctx) {
      this.state = { mode: "composer" };
      return;
    }
    const prev = this.state;
    const next = transitionMessageNav(prev, event, ctx.orderedIds(), ctx.actionsFor);
    this.state = next;
    // A deliberate exit (past newest, Tab, Escape) returns focus to the
    // composer; a cancel means focus went elsewhere on purpose.
    if (next.mode === "composer" && prev.mode !== "composer" && event.type !== "cancel") {
      ctx.focusComposer();
    }
  }
}

export const messageNavStore = new MessageNavStore();
