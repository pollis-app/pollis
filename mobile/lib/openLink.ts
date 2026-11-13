// Opening a link from message text (attacker-controlled). Rules live in lib/links.ts, shared with desktop.

import { Linking } from "react-native";
import { KeyboardController } from "react-native-keyboard-controller";
import { haptic } from "./haptics";
import { findUrls, parseLink } from "./links";
import { linkSheetStore } from "../stores/linkSheetStore";

/**
 * The system browser, never an in-app WebView, and only for a destination `parseLink`
 * accepts at this moment. No `canOpenURL` first: it needs a `<queries>` entry on Android 11+
 * and adds nothing for http(s), which every phone has a handler for.
 */
export function openInBrowser(raw: string): void {
  const target = parseLink(raw);
  if (target === null || !/^https?:\/\//.test(target.href)) {
    return;
  }
  Linking.openURL(target.href).catch(() => haptic("error"));
}

/** A tap on a link: opens it, unless its host is IDN, which first shows the real (punycode) host. */
export function tapLink(raw: string): void {
  const target = parseLink(raw);
  if (target === null) {
    return;
  }
  if (target.needsConfirm) {
    linkSheetStore.show(target, "confirm");
    return;
  }
  openInBrowser(target.href);
}

/** A long press on a link: Copy / Open. */
export function longPressLink(raw: string): void {
  const target = parseLink(raw);
  if (target === null) {
    return;
  }
  haptic("longPress");
  void KeyboardController.dismiss();
  linkSheetStore.show(target, target.needsConfirm ? "confirm" : "menu");
}

// A row is one accessible element, so its links are offered as custom actions; a few is plenty.
const MAX_LINK_ACTIONS = 5;

export interface LinkA11yAction {
  name: string;
  label: string;
  url: string;
}

/** Screen-reader actions for the links in `text` (`openLink0`, …), each labelled with its real host. */
export function linkA11yActions(
  text: string | undefined,
  label: (host: string) => string,
): LinkA11yAction[] {
  if (!text) {
    return [];
  }
  const out: LinkA11yAction[] = [];
  for (const { url } of findUrls(text) ?? []) {
    const target = parseLink(url);
    if (target === null) {
      continue;
    }
    out.push({ name: `openLink${out.length}`, label: label(target.host), url });
    if (out.length === MAX_LINK_ACTIONS) {
      break;
    }
  }
  return out;
}

/** Runs `action` if it is one of `actions`; returns whether it was. */
export function runLinkA11yAction(action: string, actions: LinkA11yAction[]): boolean {
  const hit = actions.find((a) => a.name === action);
  if (!hit) {
    return false;
  }
  tapLink(hit.url);
  return true;
}
