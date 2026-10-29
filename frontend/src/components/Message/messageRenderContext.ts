import type { Skin } from "../../utils/colorUtils";

/** Render inputs shared by every row (#874). Build with `useMemo`: a fresh object defeats the row memo. */
export interface MessageRenderContext {
  skin: Skin;
  /** Picks the dark-on-light author colors. */
  isLightBg: boolean;
  currentUserId?: string;
  /** Lowercased names an `@mention` may resolve to. */
  mentionNames: ReadonlySet<string>;
  selfName?: string;
}
