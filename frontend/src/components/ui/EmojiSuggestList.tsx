import React, { useEffect, useRef } from "react";
import { Trans, useTranslation } from "react-i18next";
import { CustomEmojiImage } from "../Emoji/CustomEmojiImage";
import type { ShortcodeEntry } from "../Emoji/emojiShortcodeQuery";

interface EmojiSuggestListProps {
  entries: ShortcodeEntry[];
  activeIndex: number;
  query: string;
  onSelect: (entry: ShortcodeEntry) => void;
  onHover: (index: number) => void;
}

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

/** Refined skin's `:shortcode:` list, `absolute` inside the composer (no portal); ChatInput owns the keys. */
export const EmojiSuggestList: React.FC<EmojiSuggestListProps> = ({
  entries,
  activeIndex,
  query,
  onSelect,
  onHover,
}) => {
  const { t } = useTranslation("common");
  const activeRef = useRef<HTMLButtonElement>(null);

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
        {/* `Trans`, so a translator can move the highlighted query. */}
        <Trans
          i18nKey="emojiSuggest.matching"
          ns="common"
          values={{ query }}
          components={{ hl: <span className="text-fg" /> }}
        />
      </div>
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
              // Keep composer focus: a blur tears down the list first.
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
