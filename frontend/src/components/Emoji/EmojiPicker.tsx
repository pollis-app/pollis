import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { observer } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { useUsableEmoji } from "../../hooks/queries/useEmoji";
import { TextInput } from "../ui/TextInput";
import { EMOJI_CATEGORIES, STANDARD_EMOJI } from "./emojiData";
import type { PickerEmoji } from "./emojiSearch";
import {
  emojiDisplayChar,
  pickerEmojiId,
  pickerEmojiInsertText,
  readRecentEmojiIds,
  readSkinTone,
  recordRecentEmojiId,
  resolveRecents,
  searchEmoji,
  writeSkinTone,
} from "./emojiSearch";
import { EmojiSection, EMOJI_COLUMNS } from "./EmojiSection";
import { EmojiCategoryRail, type RailEntry } from "./EmojiCategoryRail";
import { SkinTonePicker } from "./SkinTonePicker";
import { CustomEmojiImage } from "./CustomEmojiImage";

interface EmojiPickerProps {
  /** Called with the text to insert: a character, or a `<:name:hash>` token. */
  onSelect: (text: string) => void;
  /** Escape, or a selection when `closeOnSelect` is set. */
  onClose?: () => void;
  /** Reaction pickers close on pick; the composer's stays open for a second one. */
  closeOnSelect?: boolean;
  className?: string;
}

/** One picker section before it is flattened for keyboard navigation. */
interface Section {
  id: string;
  title: string;
  items: PickerEmoji[];
}

/**
 * The emoji picker: the full standard set, searchable, categorised, with
 * recently-used and this user's custom emoji — Discord's shape.
 *
 * **In-flow by construction.** It renders exactly where it is mounted: no
 * portal, no `position: fixed`, no backdrop. A caller that wants it floating
 * anchors it with `absolute` inside its own `relative` container (see
 * `EmojiPickerButton`), which is the pattern the existing reaction picker
 * already uses and the only one this codebase allows.
 *
 * **The custom section IS the permission rule.** It is filled from
 * `useUsableEmoji` — the Rust `list_usable_emoji`, i.e. every emoji from every
 * group the user belongs to, usable in any conversation (#848). The picker
 * deliberately does not narrow that by the conversation being typed in: doing
 * so is the intuitive-looking mistake that would stop a member of group A from
 * using A's emoji while talking in group B, which is the exact behaviour the
 * ticket asks for.
 */
export const EmojiPicker: React.FC<EmojiPickerProps> = observer(
  ({ onSelect, onClose, closeOnSelect = false, className = "" }) => {
    const { currentUser } = appStore;
    const { data: customEmoji = [] } = useUsableEmoji(currentUser?.id ?? null);

    const [query, setQuery] = useState("");
    const [toneIndex, setToneIndex] = useState(() => readSkinTone());
    const [recentIds, setRecentIds] = useState<string[]>(() => readRecentEmojiIds());
    const [preview, setPreview] = useState<PickerEmoji | null>(null);
    const [activeSection, setActiveSection] = useState<string | null>(null);

    const scrollRef = useRef<HTMLDivElement>(null);
    const [scrollRoot, setScrollRoot] = useState<HTMLElement | null>(null);

    // The lazy sections' observer root has to be a real node, and a ref does not
    // trigger a re-render — so mirror it into state once, after mount.
    useEffect(() => {
      setScrollRoot(scrollRef.current);
    }, []);

    const sections = useMemo<Section[]>(() => {
      const trimmed = query.trim();
      if (trimmed) {
        return [
          {
            id: "results",
            title: `Results for "${trimmed}"`,
            items: searchEmoji(trimmed, customEmoji),
          },
        ];
      }

      const out: Section[] = [];

      const recents = resolveRecents(recentIds, customEmoji);
      if (recents.length > 0) {
        out.push({ id: "recent", title: "Frequently used", items: recents });
      }

      // Custom emoji grouped by the group that registered them — the shape
      // Discord uses, and the one that keeps "this came from group A" legible
      // while you are typing in group B.
      const byGroup = new Map<string, PickerEmoji[]>();
      for (const emoji of customEmoji) {
        const bucket = byGroup.get(emoji.group_id) ?? [];
        bucket.push({ kind: "custom", emoji });
        byGroup.set(emoji.group_id, bucket);
      }
      for (const [groupId, items] of byGroup) {
        const name = customEmoji.find((e) => e.group_id === groupId)?.group_name;
        out.push({
          id: `custom-${groupId}`,
          title: name && name.length > 0 ? name : "Custom",
          items,
        });
      }

      for (const category of EMOJI_CATEGORIES) {
        out.push({
          id: category.id,
          title: category.label,
          items: STANDARD_EMOJI.filter((e) => e.category === category.id).map((emoji) => ({
            kind: "standard" as const,
            emoji,
          })),
        });
      }
      return out;
    }, [query, customEmoji, recentIds]);

    // Flat navigation order — the index each cell carries and what the arrow
    // keys step through.
    const flatCount = useMemo(
      () => sections.reduce((total, section) => total + section.items.length, 0),
      [sections],
    );

    const railEntries = useMemo<RailEntry[]>(
      () => sections.map((s) => ({ id: s.id, label: s.title })),
      [sections],
    );

    useEffect(() => {
      if (activeSection === null && sections.length > 0) {
        setActiveSection(sections[0].id);
      }
    }, [activeSection, sections]);

    const handleSelect = useCallback(
      (item: PickerEmoji) => {
        onSelect(pickerEmojiInsertText(item, toneIndex));
        setRecentIds(recordRecentEmojiId(pickerEmojiId(item)));
        if (closeOnSelect) {
          onClose?.();
        }
      },
      [toneIndex, onSelect, onClose, closeOnSelect],
    );

    const handleToneChange = useCallback((next: number) => {
      setToneIndex(next);
      writeSkinTone(next);
    }, []);

    const jumpTo = useCallback((sectionId: string) => {
      setActiveSection(sectionId);
      const target = scrollRef.current?.querySelector(`[data-emoji-section="${sectionId}"]`);
      target?.scrollIntoView({ block: "start" });
    }, []);

    // Keep the rail in step with the scroll position.
    const handleScroll = useCallback(() => {
      const root = scrollRef.current;
      if (!root) {
        return;
      }
      const rootTop = root.getBoundingClientRect().top;
      let current: string | null = null;
      for (const node of root.querySelectorAll<HTMLElement>("[data-emoji-section]")) {
        if (node.getBoundingClientRect().top - rootTop <= 8) {
          current = node.dataset.emojiSection ?? null;
        }
      }
      if (current) {
        setActiveSection(current);
      }
    }, []);

    /**
     * Move focus by `delta` cells in the flat order.
     *
     * Only MOUNTED cells can take focus — sections below the fold are still
     * placeholders — so a move past that boundary scrolls instead, which mounts
     * the next section, and the following keypress reaches it. Same behaviour a
     * virtualised list gives, without a virtualiser.
     */
    const moveFocus = useCallback(
      (from: number, delta: number) => {
        const next = Math.max(0, Math.min(flatCount - 1, from + delta));
        const root = scrollRef.current;
        const cell = root?.querySelector<HTMLElement>(`[data-emoji-index="${next}"]`);
        if (cell) {
          cell.focus();
          cell.scrollIntoView({ block: "nearest" });
          return;
        }
        root?.scrollBy({ top: delta > 0 ? 200 : -200 });
      },
      [flatCount],
    );

    const handleKeyDown = useCallback(
      (event: React.KeyboardEvent<HTMLDivElement>) => {
        if (event.key === "Escape") {
          event.preventDefault();
          onClose?.();
          return;
        }
        const deltas: Record<string, number> = {
          ArrowRight: 1,
          ArrowLeft: -1,
          ArrowDown: EMOJI_COLUMNS,
          ArrowUp: -EMOJI_COLUMNS,
        };
        const delta = deltas[event.key];
        if (delta === undefined) {
          return;
        }
        const active = document.activeElement as HTMLElement | null;
        const raw = active?.dataset?.emojiIndex;
        const current = raw === undefined ? -1 : Number.parseInt(raw, 10);

        event.preventDefault();
        if (current < 0) {
          // Focus is still in the search box; ArrowDown enters the grid.
          if (delta > 0) {
            moveFocus(0, 0);
          }
          return;
        }
        moveFocus(current, delta);
      },
      [moveFocus, onClose],
    );

    // Each section's base index in the flat order.
    let cursor = 0;
    const withBase = sections.map((section) => {
      const base = cursor;
      cursor += section.items.length;
      return { section, base };
    });

    return (
      <div
        data-testid="emoji-picker"
        role="dialog"
        aria-label="Emoji picker"
        onKeyDown={handleKeyDown}
        className={`panel-raised flex flex-col overflow-hidden w-[22rem] h-[24rem] ${className}`}
      >
        <div className="shrink-0 p-2 border-b border-line">
          <TextInput
            label="Search"
            value={query}
            onChange={setQuery}
            placeholder="Search emoji"
            data-testid="emoji-picker-search"
            autoFocus
          />
        </div>

        <div className="flex flex-1 min-h-0">
          <EmojiCategoryRail entries={railEntries} activeId={activeSection} onJump={jumpTo} />
          <div
            ref={scrollRef}
            onScroll={handleScroll}
            className="flex-1 min-w-0 overflow-y-auto overflow-x-hidden px-1.5 pb-1.5"
          >
            {withBase.map(({ section, base }) => (
              <EmojiSection
                key={section.id}
                id={section.id}
                title={section.title}
                items={section.items}
                toneIndex={toneIndex}
                baseIndex={base}
                onSelect={handleSelect}
                onPreview={setPreview}
                scrollRoot={scrollRoot}
              />
            ))}
            {flatCount === 0 && (
              <p
                data-testid="emoji-picker-empty"
                className="text-xs font-mono text-muted p-3 text-center"
              >
                No emoji match that.
              </p>
            )}
          </div>
        </div>

        <footer className="shrink-0 flex items-center gap-2 min-h-bar px-2 border-t border-line">
          <div className="flex items-center gap-2 min-w-0 flex-1">
            {preview ? (
              <>
                {preview.kind === "standard" ? (
                  <span className="text-xl leading-none">
                    {emojiDisplayChar(preview.emoji, toneIndex)}
                  </span>
                ) : (
                  <CustomEmojiImage
                    contentHash={preview.emoji.content_hash}
                    shortcode={preview.emoji.shortcode}
                    sizeRem={1.25}
                  />
                )}
                <span className="text-xs font-mono text-dim truncate">
                  {preview.kind === "standard"
                    ? preview.emoji.name
                    : `:${preview.emoji.shortcode}:`}
                </span>
              </>
            ) : (
              <span className="text-xs font-mono text-muted truncate">Pick an emoji</span>
            )}
          </div>
          <SkinTonePicker toneIndex={toneIndex} onChange={handleToneChange} />
        </footer>
      </div>
    );
  },
);
