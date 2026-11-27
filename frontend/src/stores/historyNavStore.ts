import { makeAutoObservable } from "mobx";
import type { RouterHistory } from "@tanstack/react-router";
import {
  canGoBack,
  canGoForward,
  finishMove,
  reduceHistoryNav,
  reduceHistoryPaths,
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
  /** Pathname per stack index, for `finishMove` (#2129). */
  private paths: (string | undefined)[] = [];
  /** Set by `pop()`: the next BACK discards the entry it left. */
  private popping = false;

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
    this.paths = reduceHistoryPaths([], "REPLACE", this.nav.index, history.location.pathname);
    attachedUnsubscribe = history.subscribe(({ location, action }) => {
      this.record(action.type, location.state.__TSR_index, location.pathname);
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
    this.paths = [];
    this.popping = false;
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

  /** How a finished form moves on to `pathname`; see `finishMove`. */
  finishMove(pathname: string): "pop" | "replace" {
    return finishMove(this.paths, this.nav.index, pathname);
  }

  /** Step back and drop the entry left behind, so Forward cannot return to it (#2129). */
  pop(): void {
    if (!this.canGoBack) {
      return;
    }
    this.popping = true;
    attachedHistory?.back();
  }

  private record(action: HistoryNavAction, index: number | undefined, pathname: string): void {
    const kind: HistoryNavAction = this.popping && action === "BACK" ? "POP" : action;
    this.popping = false;
    this.nav = reduceHistoryNav(this.nav, { action: kind, index });
    this.paths = reduceHistoryPaths(this.paths, kind, this.nav.index, pathname);
  }
}

export const historyNavStore = new HistoryNavStore();
