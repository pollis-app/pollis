// Which pushes the foreground JS handler presents. Import-free for node tests.

/** A push the app shows itself (Android rich push, #1599) or a `{d}` dismissal reaches the handler title-less; never present it. */
/** The conversation Android's push handler may treat as on screen: the open chat, only while the app is active (#1599). */
export function nativeOpenConversation(openId: string | null, appState: string): string | null {
  return appState === "active" ? openId : null;
}

export function isTitleless(content: { title?: string | null; body?: string | null }): boolean {
  return !content.title && !content.body;
}

/**
 * Present a push that reached the app in the foreground (#2056)? Never a title-less one, and never
 * one for the conversation on screen: the open chat shows the message itself, as Android's native
 * handler already decides (`RichPush.suppressForOpenConversation`). An unresolved push (`null`)
 * still shows.
 */
export function presentInForeground(
  content: { title?: string | null; body?: string | null },
  conversationId: string | null,
  openId: string | null,
  appState: string,
): boolean {
  if (isTitleless(content)) {
    return false;
  }
  const onScreen = nativeOpenConversation(openId, appState);
  return conversationId === null || onScreen === null || conversationId !== onScreen;
}
