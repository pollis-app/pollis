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
  }
}

/** Last measured px width per panel (0 = closed); not observable, read only at drag start and resize. */
const measured: Record<PanelId, number> = { sidebar: 0, rightPanel: 0 };

/** Dragged panel widths (#985); `null` = the `--side-w` token default. Loaded before first paint. */
class PanelWidthStore {
  sidebar: number | null = load("sidebar");
  rightPanel: number | null = load("rightPanel");

  constructor() {
    // `widthOf` stays tracked (not an action), as with `presenceStore.isOnline`.
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

  /** Clamp stored widths after the window shrank: sidebar first, then the right panel against it. */
  clampToWindow(windowPx: number) {
    if (this.sidebar !== null) {
      const px = clampPanelPx(this.sidebar, maxPanelPx(windowPx, measured.rightPanel));
      if (px !== this.sidebar) {
        this.setWidth("sidebar", px);
      }
    }
    // A closed sidebar takes no room.
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
