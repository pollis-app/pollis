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
  /** Draw senders' link previews (#1987); false in an unaccepted DM request. Modern look only. */
  linkPreviews: boolean;
  /** This group's aliases (#2003); empty in DMs, where the main profile shows. */
  aliases?: AliasMap;
  /** Alias names by lowercased handle, so an `@handle` mention renders the alias. */
  mentionLabels?: ReadonlyMap<string, string>;
}
