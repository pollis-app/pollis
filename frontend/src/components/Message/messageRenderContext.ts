import type { Skin } from "../../utils/colorUtils";

/**
 * The render inputs every row in a message list shares (#874).
 *
 * These are all list-wide facts — the active skin, whether the background is
 * light, who the viewer is, which `@names` resolve. Each row used to subscribe
 * to them itself, which meant one React Query observer and up to five MobX
 * reactions PER ROW for values that are identical across the whole list. On a
 * 200-message log that is ~1200 subscriptions maintained to answer the same
 * handful of questions.
 *
 * `MessageList` subscribes once and hands rows this object instead. It is
 * deliberately one object rather than half a dozen props: `MessageItem` is
 * memoised (via `observer()`), so a single stable identity is one cheap
 * reference comparison, and adding a field later does not touch the row's
 * signature.
 *
 * MUST be built with `useMemo` by the provider. A fresh object each render
 * would defeat the row memo entirely, which is the whole point of it — see
 * `MessageList`, where the memo deps are the individual fields.
 */
export interface MessageRenderContext {
  /** Active UI skin. Rows branch their STRUCTURE on this, not just color. */
  skin: Skin;
  /** True when the resolved background is light, so author colors pick the
   *  dark-on-light variant. */
  isLightBg: boolean;
  /** The viewer's user id, for `isOwn` styling and receipt visibility. */
  currentUserId?: string;
  /** Lowercased names an `@mention` may resolve to — the conversation roster
   *  plus `all` plus the viewer. Only these get tokenized; see `MessageBody`. */
  mentionNames: ReadonlySet<string>;
  /** The viewer's own lowercased username, for self-mention highlighting. */
  selfName?: string;
}
