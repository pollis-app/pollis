// Which pushes the foreground JS handler presents. Import-free for node tests.

/** A push the app shows itself (Android rich push, #1599) or a `{d}` dismissal reaches the handler title-less; never present it. */
/** The conversation Android's push handler may treat as on screen: the open chat, only while the app is active (#1599). */
export function nativeOpenConversation(openId: string | null, appState: string): string | null {
  return appState === "active" ? openId : null;
}

export function isTitleless(content: { title?: string | null; body?: string | null }): boolean {
  return !content.title && !content.body;
}
