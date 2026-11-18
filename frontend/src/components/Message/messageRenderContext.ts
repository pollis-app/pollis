import type { Skin } from "../../utils/colorUtils";
import type { AliasMap } from "../../utils/groupAlias";

/** Render inputs shared by every row (#874). Build with `useMemo`: a fresh object defeats the row memo. */
export interface MessageRenderContext {
  skin: Skin;
  /** Picks the dark-on-light author colors. */
  isLightBg: boolean;
  currentUserId?: string;
  /** Lowercased names an `@mention` may resolve to. */
  mentionNames: ReadonlySet<string>;
  selfName?: string;
  /** When the viewer joined this group (ms), so a quote of an earlier message says so; null in DMs. */
  viewerJoinedAtMs?: number | null;
  /** This group's aliases (#2003); empty in DMs, where the main profile shows. */
  aliases?: AliasMap;
  /** Alias names by lowercased handle, so an `@handle` mention renders the alias. */
  mentionLabels?: ReadonlyMap<string, string>;
  /** The group's bots (#1182), by user id and by lowercased handle; empty in DMs. */
  botIds?: ReadonlySet<string>;
  botHandles?: ReadonlySet<string>;
  /** The open group's name, for the Bot badge's "Managed by the admins of …". */
  groupName?: string;
}
