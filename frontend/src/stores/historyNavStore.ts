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

/**
 * Router history position for the breadcrumb chevrons (arithmetic in
 * `utils/historyNav`). Not persisted: history doesn't survive a relaunch.
 * Attached by `TerminalApp`, not `BreadcrumbNav`, which remounts.
 *
 * The history handle is kept OUTSIDE the observable class: MobX would
 * deep-observe the plain object and snapshot a stack it no longer tracks.
 */
let attachedHistory: RouterHistory | null = null;
let attachedUnsubscribe: (() => void) | null = null;

class HistoryNavStore {
  private nav: HistoryNavState = INITIAL_HISTORY_NAV;

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  get canGoBack(): boolean {
    return canGoBack(this.nav);
  }

  get canGoForward(): boolean {
    return canGoForward(this.nav);
  }

  /** Track `history` (replacing any previous), returning the detach. TanStack
   *  notifies on every navigation, so nothing polls. */
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

  /** One entry back, or nothing; guarded here so chevron and Escape agree. */
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
