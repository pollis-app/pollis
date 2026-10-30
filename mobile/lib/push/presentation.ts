// Which pushes the foreground JS handler presents. Import-free for node tests.

/** A push the app shows itself (Android rich push, #1599) or a `{d}` dismissal reaches the handler title-less; never present it. */
export function isTitleless(content: { title?: string | null; body?: string | null }): boolean {
  return !content.title && !content.body;
}
