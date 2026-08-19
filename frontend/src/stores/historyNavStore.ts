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
 * Where the app sits in its router history, so the breadcrumb's back/forward
 * chevrons can be greyed out honestly.
 *
 * UI state only, and deliberately not persisted: a history stack does not
 * survive a relaunch, so a remembered cursor would light a chevron pointing at
 * entries that no longer exist. The arithmetic — and the reason a forward
 * chevron cannot be driven off `history.length` — lives in `utils/historyNav`;
 * this store is only the MobX shell around it plus the subscription.
 *
 * Attached once, next to the router that owns the history (`TerminalApp`), NOT
 * from the chrome that reads it: `BreadcrumbNav` unmounts on skin changes and
 * on routes that hide the bar, and a subscription that came and went with it
 * would silently forget the forward stack every time.
 */
/**
 * The attached history and its unsubscribe, held OUTSIDE the observable class
 * on purpose.
 *
 * `RouterHistory` is a plain object, so as an instance field MobX would deep-
 * observe it: the handle becomes a copy whose `location` getter is a computed
 * over state MobX cannot see, i.e. a snapshot of a stack it no longer tracks.
 * Neither value is state anything renders from — `nav` is — so the cheapest
 * correct answer is to keep them off the observable entirely.
 */
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

  /**
   * Starts tracking `history`, replacing any previous subscription, and
   * returns the detach function.
   *
   * Event-driven by construction: TanStack's history calls every subscriber on
   * push, replace, back, forward and go, so there is nothing to poll for.
   */
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

  /** Stops tracking and forgets the stack — nothing is left to answer for. */
  detach(): void {
    attachedUnsubscribe?.();
    attachedUnsubscribe = null;
    attachedHistory = null;
    this.nav = INITIAL_HISTORY_NAV;
  }

  /**
   * One entry back, or nothing at all when there is nowhere to go.
   *
   * The guard is here rather than at each call site so the chevron and the
   * Escape shortcut cannot disagree about what "back" means.
   */
  goBack(): void {
    if (!this.canGoBack) {
      return;
    }
    attachedHistory?.back();
  }

  /** One entry forward, or nothing at all when there is nowhere to go. */
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
