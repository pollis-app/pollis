import type { Skin } from "../../utils/colorUtils";

/**
 * Render inputs every row in a message list shares (#874), subscribed once by
 * `MessageList` instead of per row. MUST be built with `useMemo`: a fresh
 * object per render defeats the row memo.
 */
export interface MessageRenderContext {
  /** Rows branch their STRUCTURE on this, not just color. */
  skin: Skin;
  /** Resolved background is light: author colors pick the dark-on-light variant. */
  isLightBg: boolean;
  currentUserId?: string;
  /** Lowercased names a mention may resolve to: roster + `all` + the viewer. */
  mentionNames: ReadonlySet<string>;
  selfName?: string;
}
