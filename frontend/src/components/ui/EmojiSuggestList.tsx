import React from "react";
import { useTranslation } from "react-i18next";
import { SuggestList } from "./SuggestList";
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

  return (
    <SuggestList
      items={entries}
      activeIndex={activeIndex}
      query={query}
      testId="emoji-suggest-list"
      label={t("emojiSuggest.listLabel")}
      matchingKey="emojiSuggest.matching"
      itemKey={(entry) => `${entry.custom ? "c" : "s"}:${entry.shortcode}`}
      optionTestId={(entry) => `emoji-option-${entry.shortcode}`}
      onSelect={onSelect}
      onHover={onHover}
      renderItem={(entry) => (
        <>
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
        </>
      )}
    />
  );
};
