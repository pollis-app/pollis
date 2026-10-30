// Android's native rich-push handler (#1599) is told two things by JS:
// - which conversation is on screen, so a push for the chat the user is reading posts nothing
//   instead of flashing a heads-up that #1508 then clears;
// - the live accent and background, so its letter avatars match the app's.

import { AppState, NativeModules, Platform } from "react-native";
import { reaction } from "mobx";
import { appStore } from "../../stores/appStore";
import { currentAccentHex, currentBackgroundHex } from "../../theme/tokens";
import { nativeOpenConversation } from "./presentation";

type PushStateModule = {
  setOpenConversation(id: string | null): void;
  setThemeColors(accentHex: string, bgHex: string): void;
};

function call(fn: () => void) {
  try {
    fn();
  } catch {
    // Worst case a push for the open chat still shows, or an avatar keeps the default colours.
  }
}

/** Starts the sync; returns a disposer. A no-op off Android or on a binary without the module. */
export function syncOpenConversationToNative(): () => void {
  const native = (NativeModules as { PollisPushState?: PushStateModule }).PollisPushState;
  if (Platform.OS !== "android" || !native) {
    return () => {};
  }
  let last: string | null | undefined;
  const push = () => {
    const next = nativeOpenConversation(appStore.openConversationId, AppState.currentState);
    if (next !== last) {
      last = next;
      call(() => native.setOpenConversation(next));
    }
    // The accent can only change in the app, so leaving it is when native needs the latest.
    if (AppState.currentState !== "active") {
      call(() => native.setThemeColors(currentAccentHex(), currentBackgroundHex()));
    }
  };
  push();
  call(() => native.setThemeColors(currentAccentHex(), currentBackgroundHex()));
  const disposeReaction = reaction(() => appStore.openConversationId, push);
  const sub = AppState.addEventListener("change", push);
  return () => {
    disposeReaction();
    sub.remove();
    call(() => native.setOpenConversation(null));
  };
}
