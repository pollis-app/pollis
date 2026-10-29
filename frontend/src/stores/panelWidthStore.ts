import { makeAutoObservable } from "mobx";
import {
  RIGHT_PANEL_WIDTH_KEY,
  SIDEBAR_WIDTH_KEY,
  clampPanelPx,
  maxPanelPx,
  parseStoredWidth,
} from "../components/Layout/panelWidth";

export type PanelId = "sidebar" | "rightPanel";

const STORAGE_KEY: Record<PanelId, string> = {
  sidebar: SIDEBAR_WIDTH_KEY,
  rightPanel: RIGHT_PANEL_WIDTH_KEY,
};

function load(id: PanelId): number | null {
  try {
    return parseStoredWidth(localStorage.getItem(STORAGE_KEY[id]));
  } catch {
      // localStorage unavailable: no answer.
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
    // localStorage unavailable / quota exceeded: ignore.
  }
}

/**
 * Each panel's last measured px width (0 while closed), reported by
 * `usePanelWidth`: only the DOM knows the `--side-w` token in px. NOT
 * observable, or every report would re-render both panels; read at drag start
 * and on window resize only.
 */
const measured: Record<PanelId, number> = { sidebar: 0, rightPanel: 0 };

/**
 * Dragged widths for the sidebar and right panel (#985). `null` renders the
 * `--side-w` token (`w-side`); a number is an inline px width. Read from
 * `localStorage` at module eval so a restored layout never flashes. See
 * `components/Layout/panelWidth.ts`.
 */
class PanelWidthStore {
  sidebar: number | null = load("sidebar");
  rightPanel: number | null = load("rightPanel");

  constructor() {
    // `widthOf` stays a plain method so its reads are tracked (see presenceStore).
    makeAutoObservable(this, { widthOf: false }, { autoBind: true });
  }

  widthOf(id: PanelId): number | null {
    return id === "sidebar" ? this.sidebar : this.rightPanel;
  }

  /** A user-chosen width, or `null` for the token default. */
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

  maxFor(id: PanelId, windowPx: number): number {
    const other: PanelId = id === "sidebar" ? "rightPanel" : "sidebar";
    return maxPanelPx(windowPx, measured[other]);
  }

  /**
   * Pull stored widths back inside the window after it shrank: sidebar first,
   * since its width bounds the right panel. Token-default panels are untouched.
   */
  clampToWindow(windowPx: number) {
    if (this.sidebar !== null) {
      const px = clampPanelPx(this.sidebar, maxPanelPx(windowPx, measured.rightPanel));
      if (px !== this.sidebar) {
        this.setWidth("sidebar", px);
      }
    }
    // A closed sidebar takes no room (`measured` is 0 while closed).
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
