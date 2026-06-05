import { makeAutoObservable } from "mobx";

// Per-conversation queue of roster-change banners (member joined, member
// left, device added, device removed). Pushed by the `roster_changed`
// realtime event the backend emits after `reconcile_group_mls_impl`
// produces a non-empty commit.
//
// The store key is `conversation_id` because banners are conversation-
// scoped: a join in #engineering does not surface in #design. Banners
// stay in memory for the session (no persistence) and time out so a
// long-lived window doesn't accumulate stale notices. The MessageList
// in that conversation interleaves them with messages by timestamp.

export type RosterBannerKind =
  | { kind: "joined"; user_id: string }
  | { kind: "left"; user_id: string }
  | { kind: "device_added"; user_id: string; device_id: string }
  | { kind: "device_removed"; user_id: string; device_id: string };

export interface RosterBanner extends Record<string, unknown> {
  /** Stable id so React keys don't churn across re-renders. */
  id: string;
  /** Local wall-clock observation time. Drives chronological ordering
   *  alongside message timestamps in the channel timeline. */
  observed_at_ms: number;
  /** MLS epoch the commit landed at. Surfaced in the banner so a power
   *  user can debug ordering against the commit log. */
  epoch: number;
  /** What happened. The kind is a tagged union so the renderer can
   *  branch cleanly on shape rather than a string compare. */
  payload: RosterBannerKind;
}

// Cap per-conversation history so a noisy reconcile loop can't pin
// arbitrary memory. 200 is well above any realistic single-session
// roster churn; older banners drop off the front.
const MAX_PER_CONVERSATION = 200;

class RosterChangeStore {
  /** conversation_id → list of banners, oldest first. */
  byConversation: Record<string, RosterBanner[]> = {};

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  push(conversation_id: string, banners: RosterBanner[]) {
    if (banners.length === 0) {
      return;
    }
    const existing = this.byConversation[conversation_id] ?? [];
    const next = [...existing, ...banners];
    const trimmed =
      next.length > MAX_PER_CONVERSATION
        ? next.slice(next.length - MAX_PER_CONVERSATION)
        : next;
    this.byConversation = {
      ...this.byConversation,
      [conversation_id]: trimmed,
    };
  }

  clearConversation(conversation_id: string) {
    if (!(conversation_id in this.byConversation)) {
      return;
    }
    const next = { ...this.byConversation };
    delete next[conversation_id];
    this.byConversation = next;
  }

  clearAll() {
    this.byConversation = {};
  }
}

export const rosterChangeStore = new RosterChangeStore();
