import { makeAutoObservable } from "mobx";
import {
  RIGHT_PANEL_WIDTH_KEY,
  SIDEBAR_WIDTH_KEY,
  clampPanelPx,
  maxPanelPx,
  parseStoredWidth,
} from "../components/Layout/panelWidth";

/** The two resizable panels, named once so a key can never be mistyped. */
export type PanelId = "sidebar" | "rightPanel";

const STORAGE_KEY: Record<PanelId, string> = {
  sidebar: SIDEBAR_WIDTH_KEY,
  rightPanel: RIGHT_PANEL_WIDTH_KEY,
};

function load(id: PanelId): number | null {
  try {
    return parseStoredWidth(localStorage.getItem(STORAGE_KEY[id]));
  } catch {
    // localStorage unavailable (private mode, disabled storage) — no answer.
    return null;
  }
}

function save(id: PanelId, px: number | null): void {
  try {
    if (px === null) {
      localStorage.removeItem(STORAGE_KEY[id]);
      return;
    }
    localStorage.setItem(STORAGE_KEY[id], String(px));
  } catch {
    // localStorage unavailable / quota exceeded — fall through silently.
  }
}

/**
 * Each panel's LAST MEASURED pixel width, or 0 while it is closed.
 *
 * Module-level and deliberately NOT observable. It is needed because a panel
 * still on its token default has no stored number to bound the other one
 * with — `--side-w` is a rem measure, so only the DOM knows what it currently
 * comes to — and the panels report it after every render (see
 * `usePanelWidth`), which is also how a closed panel gets back to 0. Making it
 * reactive would re-render both panels every time one of them reported its own
 * size back; it is read at drag start and on window resize, never during
 * render.
 */
const measured: Record<PanelId, number> = { sidebar: 0, rightPanel: 0 };

/**
 * Dragged widths for the left sidebar and the right panel (#985).
 *
 * `null` means "this device has no answer", and the panel renders at the
 * `--side-w` design token via the `w-side` utility — a per-skin rem measure,
 * so the default keeps tracking the skin and the font-size preference exactly
 * as it did before this store existed. A number is a user-chosen pixel width
 * and wins as an inline style. Double-clicking a handle writes `null`, which
 * is what makes "reset to default" mean the token rather than a hardcoded
 * number.
 *
 * Widths are read from `localStorage` in the field initialisers — at module
 * eval, before first paint — so a restored layout never flashes the default
 * width. Why per device and not in the synced preferences blob or the
 * database: see `components/Layout/panelWidth.ts`.
 */
class PanelWidthStore {
  sidebar: number | null = load("sidebar");
  rightPanel: number | null = load("rightPanel");

  constructor() {
    // `widthOf` is excluded from auto-annotation for the same reason
    // `presenceStore.isOnline` is: actions run untracked, so as an action its
    // read of `sidebar` / `rightPanel` would be invisible to the observing
    // panel and a committed drag would not repaint.
    makeAutoObservable(this, { widthOf: false }, { autoBind: true });
  }

  /** The stored width for `id`, or null when it is still on the token default. */
  widthOf(id: PanelId): number | null {
    return id === "sidebar" ? this.sidebar : this.rightPanel;
  }

  /** Records a user-chosen width, or `null` to fall back to the token default. */
  setWidth(id: PanelId, px: number | null) {
    if (id === "sidebar") {
      this.sidebar = px;
    } else {
      this.rightPanel = px;
    }
    save(id, px);
  }

  reportMeasured(id: PanelId, px: number) {
    measured[id] = px;
  }

  /** The widest `id` may currently be drawn, given the window and its sibling. */
  maxFor(id: PanelId, windowPx: number): number {
    const other: PanelId = id === "sidebar" ? "rightPanel" : "sidebar";
    return maxPanelPx(windowPx, measured[other]);
  }

  /**
   * Pull both stored widths back inside the bounds after the window shrank.
   *
   * Sequential rather than parallel: the sidebar is clamped first and its new
   * width is what bounds the right panel, so one pass cannot leave a pair that
   * together overflow the window. Panels still on the token default are left
   * alone — their width is a rem measure this store cannot rewrite, and it is
   * the width the app shipped with.
   */
  clampToWindow(windowPx: number) {
    if (this.sidebar !== null) {
      const px = clampPanelPx(this.sidebar, maxPanelPx(windowPx, measured.rightPanel));
      if (px !== this.sidebar) {
        this.setWidth("sidebar", px);
      }
    }
    // A closed sidebar takes no room whatever it remembers, so `measured` (0
    // while it is closed) decides whether its stored width counts at all.
    const sidebarPx = measured.sidebar === 0 ? 0 : this.sidebar ?? measured.sidebar;
    if (this.rightPanel !== null) {
      const px = clampPanelPx(this.rightPanel, maxPanelPx(windowPx, sidebarPx));
      if (px !== this.rightPanel) {
        this.setWidth("rightPanel", px);
      }
    }
  }
}

export const panelWidthStore = new PanelWidthStore();
