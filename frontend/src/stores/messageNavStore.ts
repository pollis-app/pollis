import { makeAutoObservable } from "mobx";
import {
  transitionMessageNav,
  type MessageNavAction,
  type MessageNavEvent,
  type MessageNavState,
} from "../utils/messageNav";

/**
 * What the active chat surface lends the navigator. Registered by the ONE
 * `MessageList` that opts into keyboard navigation (the main chat) while it is
 * mounted; every callback reads live component state so the store never holds
 * a stale copy of the message list.
 */
export interface MessageNavContext {
  /** Rendered, navigable message ids, oldest → newest. */
  orderedIds: () => string[];
  /** The action bar of one message, in visual order, from the rendered DOM. */
  actionsFor: (messageId: string) => MessageNavAction[];
  /** Return focus to the composer input. */
  focusComposer: () => void;
}

/**
 * Live keyboard-navigation state for the chat log (UI selection state only —
 * see `utils/messageNav.ts` for the machine and the invariants). All mutation
 * funnels through `dispatch`, which runs the pure transition against the
 * registered surface context.
 */
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
    // Exiting the log deliberately (down past the newest, Tab, Escape) hands
    // focus back to the composer. A cancel means focus already went somewhere
    // else on purpose — never steal it back.
    if (next.mode === "composer" && prev.mode !== "composer" && event.type !== "cancel") {
      ctx.focusComposer();
    }
  }
}

export const messageNavStore = new MessageNavStore();
