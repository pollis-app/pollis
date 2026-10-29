import { makeAutoObservable } from "mobx";
import type { RouterHistory } from "@tanstack/react-router";
import {
  canGoBack,
  canGoForward,
  reduceHistoryNav,
  syncHistoryNav,
  type HistoryNavAction,
  type HistoryNavState,
  INITIAL_HISTORY_NAV,
} from "../utils/historyNav";

/** Held outside the observable class: MobX would deep-observe `RouterHistory` into a stale snapshot. */
let attachedHistory: RouterHistory | null = null;
let attachedUnsubscribe: (() => void) | null = null;

class HistoryNavStore {
  private nav: HistoryNavState = INITIAL_HISTORY_NAV;

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  /** True when an entry exists behind the cursor. */
  get canGoBack(): boolean {
    return canGoBack(this.nav);
  }

  /** True when an entry exists ahead of the cursor. */
  get canGoForward(): boolean {
    return canGoForward(this.nav);
  }

  /** Track `history` (replacing any previous one); returns the detach. */
  attach(history: RouterHistory): () => void {
    this.detach();
    attachedHistory = history;
    this.nav = syncHistoryNav(history.location.state.__TSR_index);
    attachedUnsubscribe = history.subscribe(({ location, action }) => {
      this.record(action.type, location.state.__TSR_index);
    });
    return () => {
      this.detach();
    };
  }

  detach(): void {
    attachedUnsubscribe?.();
    attachedUnsubscribe = null;
    attachedHistory = null;
    this.nav = INITIAL_HISTORY_NAV;
  }

  /** One entry back if possible; guarded here so chevron and Escape agree. */
  goBack(): void {
    if (!this.canGoBack) {
      return;
    }
    attachedHistory?.back();
  }

  goForward(): void {
    if (!this.canGoForward) {
      return;
    }
    attachedHistory?.forward();
  }

  private record(action: HistoryNavAction, index: number | undefined): void {
    this.nav = reduceHistoryNav(this.nav, { action, index });
  }
}

export const historyNavStore = new HistoryNavStore();
