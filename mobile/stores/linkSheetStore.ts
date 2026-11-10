// The open link sheet (Copy / Open, or the IDN confirm), in memory only; `LinkSheetHost` draws it.

import { makeAutoObservable } from "mobx";
import type { LinkTarget } from "../lib/links";

type LinkSheetMode = "confirm" | "menu";

class LinkSheetStore {
  target: LinkTarget | null = null;
  mode: LinkSheetMode = "menu";

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  show(target: LinkTarget, mode: LinkSheetMode) {
    this.target = target;
    this.mode = mode;
  }

  close() {
    this.target = null;
  }
}

export const linkSheetStore = new LinkSheetStore();
