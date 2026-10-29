import { makeAutoObservable } from "mobx";
import {
  transitionMessageNav,
  type MessageNavAction,
  type MessageNavEvent,
  type MessageNavState,
} from "../utils/messageNav";

/** Callbacks lent by the one navigable `MessageList`; they read live state. */
export interface MessageNavContext {
  /** Rendered, navigable message ids, oldest → newest. */
  orderedIds: () => string[];
  /** The action bar of one message, in visual order, from the rendered DOM. */
  actionsFor: (messageId: string) => MessageNavAction[];
  /** Return focus to the composer input. */
  focusComposer: () => void;
}

/** Chat-log keyboard nav state; all mutation goes through `dispatch` (see `utils/messageNav.ts`). */
class MessageNavStore {
  state: MessageNavState = { mode: "composer" };
  private ctx: MessageNavContext | null = null;

  constructor() {
    makeAutoObservable(this);
  }

  /** True while focus logically lives in the message log. */
  get active(): boolean {
    return this.state.mode !== "composer";
  }

  /** Mounting surface lends its context; any in-flight navigation resets. */
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
    // Deliberate exits refocus the composer; a cancel never steals focus.
    if (next.mode === "composer" && prev.mode !== "composer" && event.type !== "cancel") {
      ctx.focusComposer();
    }
  }
}

export const messageNavStore = new MessageNavStore();
