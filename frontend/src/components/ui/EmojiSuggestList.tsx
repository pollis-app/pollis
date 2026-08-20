import React, { useEffect, useRef } from "react";
import { Trans, useTranslation } from "react-i18next";
import { CustomEmojiImage } from "../Emoji/CustomEmojiImage";
import type { ShortcodeEntry } from "../Emoji/emojiShortcodeQuery";

interface EmojiSuggestListProps {
  entries: ShortcodeEntry[];
  /** Index of the highlighted row — moved by the arrow keys in ChatInput. */
  activeIndex: number;
  /** What the user has typed after the ':', used to bold the matched run. */
  query: string;
  onSelect: (entry: ShortcodeEntry) => void;
  onHover: (index: number) => void;
}

/**
 * Highlight the part of the shortcode the user has actually typed, the same
 * per-keystroke feedback `MentionSuggestList` gives.
 */
const MatchedCode: React.FC<{ shortcode: string; query: string }> = ({
  shortcode,
  query,
}) => {
  const at = query ? shortcode.indexOf(query.toLowerCase()) : -1;
  if (at < 0) {
    return <>:{shortcode}:</>;
  }
  return (
    <>
      :{shortcode.slice(0, at)}
      <span className="font-semibold text-accent">
        {shortcode.slice(at, at + query.length)}
      </span>
      {shortcode.slice(at + query.length)}:
    </>
  );
};

/**
 * Refined skin's `:shortcode:` autocomplete — the Slack/Discord inline list
 * that sits directly above the composer, the twin of `MentionSuggestList`.
 * Arrow keys move, Enter/Tab accept, Esc dismisses; all of that is driven by
 * ChatInput's key handler, which owns the textarea.
 *
 * Positioning is `absolute` INSIDE the composer's own relative container
 * (`bottom-full`), not a portal to `document.body` and not `position: fixed`.
 * It overhangs the message list as a normal part of the composer's layout,
 * which is what Slack does and what the no-modals rule allows.
 */
export const EmojiSuggestList: React.FC<EmojiSuggestListProps> = ({
  entries,
  activeIndex,
  query,
  onSelect,
  onHover,
}) => {
  const { t } = useTranslation("common");
  const activeRef = useRef<HTMLButtonElement>(null);

  // Keep the highlighted row in view when the arrow keys walk past the edge
  // of the scroll box.
  useEffect(() => {
    activeRef.current?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  if (entries.length === 0) {
    return null;
  }

  return (
    <div
      data-testid="emoji-suggest-list"
      role="listbox"
      aria-label={t("emojiSuggest.listLabel")}
      className="absolute bottom-full left-0 right-0 z-20 mb-1 mx-2 overflow-hidden rounded-panel border border-line bg-surface-raised"
    >
      <div className="px-3 py-1.5 border-b border-line text-2xs text-muted select-none">
        {/* `Trans` rather than two keys: the typed query is highlighted
            mid-sentence, and a translator must be free to move it. */}
        <Trans
          i18nKey="emojiSuggest.matching"
          ns="common"
          values={{ query }}
          components={{ hl: <span className="text-fg" /> }}
        />
      </div>
      {/* Caps the list at roughly six rows (max-h-60 is 15rem, so it tracks
          the user's font setting rather than a pixel height). */}
      <div className="max-h-60 overflow-y-auto py-1">
        {entries.map((entry, i) => {
          const isActive = i === activeIndex;
          return (
            <button
              key={`${entry.custom ? "c" : "s"}:${entry.shortcode}`}
              ref={isActive ? activeRef : undefined}
              type="button"
              role="option"
              aria-selected={isActive}
              data-testid={`emoji-option-${entry.shortcode}`}
              data-active={isActive ? "true" : undefined}
              // The composer must keep focus — a blur would tear down the
              // list before the click ever lands.
              onMouseDown={(e) => {
                e.preventDefault();
                onSelect(entry);
              }}
              onMouseEnter={() => onHover(i)}
              className={`w-full flex items-center gap-2 px-3 py-1.5 text-start text-sm hover:bg-hover ${
                isActive ? "bg-active" : ""
              }`}
            >
              <span className="w-6 flex-shrink-0 text-center text-base leading-none">
                {entry.custom && entry.contentHash ? (
                  <CustomEmojiImage
                    contentHash={entry.contentHash}
                    shortcode={entry.shortcode}
                    sizeRem={1.25}
                  />
                ) : (
                  entry.char
                )}
              </span>
              <span className="truncate font-mono text-fg">
                <MatchedCode shortcode={entry.shortcode} query={query} />
              </span>
              <span className="truncate text-xs text-muted">{entry.label}</span>
            </button>
          );
        })}
      </div>
    </div>
  );
};
