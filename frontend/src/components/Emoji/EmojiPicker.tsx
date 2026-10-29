import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { appStore } from "../../stores/appStore";
import { horizontalArrowStep } from "../../utils/direction";
import { useUsableEmoji } from "../../hooks/queries/useEmoji";
import { TextInput } from "../ui/TextInput";
import { EMOJI_CATEGORIES, STANDARD_EMOJI } from "./emojiData";
import { emojiDisplayName } from "./emojiAnnotations";
import { useEmojiAnnotations } from "./useEmojiAnnotations";
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
  onSelect: (text: string) => void;
  onClose?: () => void;
  closeOnSelect?: boolean;
  className?: string;
  /** Pixel cap, computed at open time, when the viewport can't seat the panel. */
  maxHeight?: number;
}

interface Section {
  id: string;
  title: string;
  items: PickerEmoji[];
}

/** The emoji picker, in-flow (no portal/fixed/backdrop). Custom emoji come from every group the user is in, never narrowed by conversation (#848). */
export const EmojiPicker: React.FC<EmojiPickerProps> = observer(
  ({ onSelect, onClose, closeOnSelect = false, className = "", maxHeight }) => {
    const { t, i18n } = useTranslation("emoji");
    const { currentUser } = appStore;
    const { data: customEmoji = [] } = useUsableEmoji(currentUser?.id ?? null);
    const annotations = useEmojiAnnotations(i18n.language);

    const [query, setQuery] = useState("");
    const [toneIndex, setToneIndex] = useState(() => readSkinTone());
    const [recentIds, setRecentIds] = useState<string[]>(() => readRecentEmojiIds());
    const [preview, setPreview] = useState<PickerEmoji | null>(null);
    const [activeSection, setActiveSection] = useState<string | null>(null);

    const scrollRef = useRef<HTMLDivElement>(null);
    const [scrollRoot, setScrollRoot] = useState<HTMLElement | null>(null);

    // Observer root must be a real node; mirror the ref into state.
    useEffect(() => {
      setScrollRoot(scrollRef.current);
    }, []);

    const sections = useMemo<Section[]>(() => {
      const trimmed = query.trim();
      if (trimmed) {
        return [
          {
            id: "results",
            title: t("picker.resultsFor", { query: trimmed }),
            items: searchEmoji(trimmed, customEmoji, annotations),
          },
        ];
      }

      const out: Section[] = [];

      const recents = resolveRecents(recentIds, customEmoji);
      if (recents.length > 0) {
        out.push({ id: "recent", title: t("picker.recent"), items: recents });
      }

      // Custom emoji grouped by their group, Discord-style.
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
          title: name && name.length > 0 ? name : t("picker.custom"),
          items,
        });
      }

      for (const category of EMOJI_CATEGORIES) {
        out.push({
          id: category.id,
          title: t(`categories.${category.id}`),
          items: STANDARD_EMOJI.filter((e) => e.category === category.id).map((emoji) => ({
            kind: "standard" as const,
            emoji,
          })),
        });
      }
      return out;
    }, [query, customEmoji, recentIds, annotations, t]);

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

    /** Only mounted cells take focus, so a move past the fold scrolls to mount the next section. */
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
        // Inline axis: ArrowLeft is "next" under RTL (utils/direction.ts).
        const horizontal = horizontalArrowStep(event.key, event.currentTarget);
        const verticalDeltas: Record<string, number> = {
          ArrowDown: EMOJI_COLUMNS,
          ArrowUp: -EMOJI_COLUMNS,
        };
        const delta =
          horizontal !== 0 ? horizontal : verticalDeltas[event.key];
        if (delta === undefined) {
          return;
        }
        const active = document.activeElement as HTMLElement | null;
        const raw = active?.dataset?.emojiIndex;
        const current = raw === undefined ? -1 : Number.parseInt(raw, 10);

        event.preventDefault();
        if (current < 0) {
          if (delta > 0) {
            moveFocus(0, 0);
          }
          return;
        }
        moveFocus(current, delta);
      },
      [moveFocus, onClose],
    );

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
        aria-label={t("picker.label")}
        onKeyDown={handleKeyDown}
        // `max-height` beats `height`; lost rows become scroll.
        style={maxHeight === undefined ? undefined : { maxHeight }}
        className={`panel-raised flex flex-col overflow-hidden w-[22rem] h-[24rem] ${className}`}
      >
        <div className="shrink-0 p-2 border-b border-line">
          <TextInput
            label={t("picker.searchLabel")}
            value={query}
            onChange={setQuery}
            placeholder={t("picker.searchPlaceholder")}
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
                annotations={annotations}
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
                {t("picker.empty")}
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
                    ? emojiDisplayName(preview.emoji, annotations)
                    : `:${preview.emoji.shortcode}:`}
                </span>
              </>
            ) : (
              <span className="text-xs font-mono text-muted truncate">{t("picker.previewHint")}</span>
            )}
          </div>
          <SkinTonePicker toneIndex={toneIndex} onChange={handleToneChange} />
        </footer>
      </div>
    );
  },
);
