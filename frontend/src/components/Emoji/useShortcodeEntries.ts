/**
 * The composer's `:shortcode:` index: every custom emoji the user may send,
 * followed by every standard alias.
 *
 * Two halves with two different lifetimes, which is why they are assembled
 * here rather than in one module:
 *
 *   - CUSTOM comes from `useUsableEmoji` — the same query the picker uses, so
 *     it is shared and cached — and is fetched eagerly. It has to be: typing
 *     `:tada:` straight out must resolve to the group's own emoji, and a list
 *     that arrives a beat later would silently hand the sender the Unicode one.
 *   - STANDARD is the 1500-entry table, dynamically imported the first time a
 *     colon actually opens a query, so the composer keeps `emojiData.ts` out of
 *     the startup chunk exactly as #874 intends.
 *
 * Custom entries come first in the array, which is a redundancy rather than the
 * rule: `rankShortcodeEntries` and `resolveShortcode` both prefer custom on
 * their own, and neither depends on input order.
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
    // Rebuilt on every activation, not once: the build reads the stored skin
    // tone, and a tone the user changed in the picker has to take effect in
    // the composer without a reload. The map is over ~1600 rows and runs when
    // a colon is typed, which is nowhere near a frame budget.
    import("./emojiShortcodeIndex")
      .then((module) => {
        if (!cancelled) {
          setStandard(module.standardShortcodeEntries());
        }
      })
      .catch(() => {
        // A failed chunk load costs the standard half of the suggestions and
        // nothing else — custom emoji still complete, and the composer still
        // sends. Never let it throw into the render path.
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
