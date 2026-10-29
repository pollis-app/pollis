/**
 * The composer's `:shortcode:` index: custom emoji, then standard aliases.
 *
 * Custom (`useUsableEmoji`) is fetched eagerly: typing `:tada:` straight out
 * must resolve to the group's own emoji. Standard is imported on the first
 * colon, keeping `emojiData.ts` out of the startup chunk (#874).
 */

import { useEffect, useMemo, useState } from "react";
import { useObserver } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { useUsableEmoji } from "../../hooks/queries/useEmoji";
import { emojiTokenText } from "./emojiTokens";
import type { ShortcodeEntry } from "./emojiShortcodeQuery";

export function useShortcodeEntries(active: boolean): ShortcodeEntry[] {
  const currentUser = useObserver(() => appStore.currentUser);
  const { data: customEmoji } = useUsableEmoji(currentUser?.id ?? null);
  const [standard, setStandard] = useState<ShortcodeEntry[]>([]);

  useEffect(() => {
    if (!active) {
      return;
    }
    let cancelled = false;
    // Rebuilt on every activation so a skin tone changed in the picker applies.
    import("./emojiShortcodeIndex")
      .then((module) => {
        if (!cancelled) {
          setStandard(module.standardShortcodeEntries());
        }
      })
      .catch(() => {
        // A failed chunk load only costs the standard suggestions.
      });
    return () => {
      cancelled = true;
    };
  }, [active]);

  return useMemo(() => {
    const custom: ShortcodeEntry[] = (customEmoji ?? []).map((emoji) => ({
      shortcode: emoji.shortcode,
      insertText: emojiTokenText(emoji.shortcode, emoji.content_hash),
      custom: true,
      char: null,
      contentHash: emoji.content_hash,
      label: emoji.group_name,
    }));
    return [...custom, ...standard];
  }, [customEmoji, standard]);
}
