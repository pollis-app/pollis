import React, { useState, useEffect, useRef, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useSearchMessages } from "../../hooks/queries/useSearchMessages";
import { formatShortDateTime } from "../../utils/format";
import { Button } from "../ui/Button";
import { EmojiText } from "../Emoji/EmojiText";
import { MediaTile } from "../Layout/RightPanel/MediaTile";
import { parseContent } from "../../hooks/queries/useMessages";
import { shellOpen } from "../../bridge";
import type { SearchCorpus, SearchResult, SearchSnippet, SearchSort } from "../../types";

/**
 * The `/learn` entry explaining why search is on-device.
 *
 * A trust asset, not an apology: the server holds ciphertext, so it CANNOT
 * search for you, and the limits of the on-device corpus follow from that
 * rather than from an unfinished feature.
 */
const LEARN_ON_DEVICE_SEARCH_URL = "https://pollis.com/learn#on-device-search";

// ─── Helpers ──────────────────────────────────────────────────────────────────

/**
 * Render a snippet with the ranges Rust computed.
 *
 * The old highlighter built a `g`-flagged regex from the raw query and reused
 * it across `.test()` calls, so its `lastIndex` carried between parts and every
 * other match failed to highlight. There is no regex here at all: the backend
 * returns `[start, end)` pairs in UTF-16 code units — i.e. plain string indices
 * — and this slices between them. It is also what keeps `<mark>` out of
 * `dangerouslySetInnerHTML`, so a message body can never become markup.
 *
 * Each slice is then rendered through `EmojiText`, the same component the
 * message log uses, so a `<:shortcode:hash>` token shows the image instead of
 * its raw source text. A highlight range that happens to cut a token in half
 * simply leaves that fragment literal — `splitEmojiSegments` only accepts
 * well-formed tokens — which degrades to today's behaviour rather than
 * breaking.
 */
export const HighlightedSnippet: React.FC<{ snippet: SearchSnippet }> = ({ snippet }) => {
  const parts = useMemo(() => {
    const out: { text: string; mark: boolean }[] = [];
    let cursor = 0;
    for (const [start, end] of snippet.highlights) {
      if (start < cursor || end > snippet.text.length || end <= start) {
        continue;
      }
      if (start > cursor) {
        out.push({ text: snippet.text.slice(cursor, start), mark: false });
      }
      out.push({ text: snippet.text.slice(start, end), mark: true });
      cursor = end;
    }
    if (cursor < snippet.text.length) {
      out.push({ text: snippet.text.slice(cursor), mark: false });
    }
    return out;
  }, [snippet]);

  return (
    <span>
      {parts.map((part, i) =>
        part.mark ? (
          <mark key={i} data-testid="search-highlight" className="bg-accent-muted text-accent-bright rounded-sm px-0.5">
            <EmojiText text={part.text} />
          </mark>
        ) : (
          <span key={i}>
            <EmojiText text={part.text} />
          </span>
        ),
      )}
    </span>
  );
};

/** How many image thumbnails a single hit is willing to show. A result row is
 *  a preview, not the message — the rest are one click away in the log. */
const MAX_RESULT_THUMBNAILS = 3;

/**
 * The image half of a hit's preview.
 *
 * The snippet is text, so an attached picture reached the row as its filename
 * and nothing else. The full content is already on the result (the backend
 * returns it for the snippet), so the attachment envelope can be unwrapped
 * here with the same `parseContent` the message log uses, and each image drawn
 * with `MediaTile` — which resolves bytes through the loopback media server
 * and falls back to an icon on failure. Non-image attachments keep their
 * filename in the snippet and add nothing here.
 */
const ResultThumbnails: React.FC<{ result: SearchResult }> = ({ result }) => {
  const images = useMemo(() => {
    if (!result.has_attachment) {
      return [];
    }
    return (parseContent(result.content).attachments ?? [])
      .filter((a) => a.content_type.startsWith("image/"))
      .slice(0, MAX_RESULT_THUMBNAILS);
  }, [result.has_attachment, result.content]);

  if (images.length === 0) {
    return null;
  }

  return (
    <div className="mt-1.5 flex gap-1.5" data-testid="search-result-thumbnails">
      {images.map((attachment) => (
        <div key={attachment.id} className="w-12 flex-shrink-0">
          <MediaTile attachment={attachment} />
        </div>
      ))}
    </div>
  );
};

/** `#general` in Acme, or `@bob`, or the bare id when this device has never
 *  listed the conversation. */
function conversationLabel(result: SearchResult): string {
  if (result.conversation_kind === "channel" && result.conversation_name) {
    return result.group_name
      ? `${result.group_name} / #${result.conversation_name}`
      : `#${result.conversation_name}`;
  }
  if (result.conversation_kind === "dm" && result.conversation_name) {
    return `@${result.conversation_name}`;
  }
  return result.conversation_id;
}

// ─── Props ────────────────────────────────────────────────────────────────────

interface SearchViewProps {
  /** Open the conversation and jump to the message. */
  onOpenResult: (result: SearchResult) => void;
  /** Take the user to the retention setting from the corpus footer. */
  onOpenRetentionSettings?: () => void;
  /** Query handed over from the Cmd+K navigator, so the page opens searching. */
  initialQuery?: string;
}

// ─── SearchView ───────────────────────────────────────────────────────────────

export const SearchView: React.FC<SearchViewProps> = ({
  onOpenResult,
  onOpenRetentionSettings,
  initialQuery = "",
}) => {
  const { t } = useTranslation("search");
  const [inputValue, setInputValue] = useState(initialQuery);
  // Seeded, not debounced: a handed-off query is already what the user meant,
  // so it should not spend 300 ms looking like it did nothing.
  const [debouncedQuery, setDebouncedQuery] = useState(initialQuery);
  // Null means "let the backend decide" — relevance globally, recency inside a
  // conversation. It only becomes a value once the user overrides it.
  const [sort, setSort] = useState<SearchSort | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // Auto-focus the input when the view mounts
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // Debounce the input: wait 300 ms after the user stops typing before searching
  useEffect(() => {
    if (debounceRef.current) {
      clearTimeout(debounceRef.current);
    }
    debounceRef.current = setTimeout(() => {
      setDebouncedQuery(inputValue);
    }, 300);

    return () => {
      if (debounceRef.current) {
        clearTimeout(debounceRef.current);
      }
    };
  }, [inputValue]);

  const hasMinLength = debouncedQuery.trim().length >= 2;
  const { data, isFetching, fetchNextPage, hasNextPage, isFetchingNextPage } =
    useSearchMessages(debouncedQuery, { sort });

  const results = useMemo(
    () => data?.pages.flatMap((page) => page.results) ?? [],
    [data],
  );
  const total = data?.pages[0]?.total ?? 0;
  const corpus: SearchCorpus | null = data?.pages[0]?.corpus ?? null;
  // The ordering the backend actually applied, which is what the toggle shows
  // when the user has not overridden it.
  const activeSort: SearchSort = sort ?? data?.pages[0]?.sort ?? "relevant";

  const renderEmptyState = () => {
    if (!debouncedQuery.trim() || !hasMinLength) {
      return (
        <p
          data-testid="search-empty-hint"
          className="text-xs font-mono text-center text-muted pt-8"
        >
          {t("view.emptyHint")}
        </p>
      );
    }

    if (isFetching) {
      return (
        <p className="text-xs font-mono text-center text-muted pt-8">
          {t("view.searching")}
        </p>
      );
    }

    // Not "No results". A message can be missing for five reasons that have
    // nothing to do with the query, and every one of them is a property of how
    // end-to-end encryption works rather than a bug (#850 §6).
    return (
      <div
        data-testid="search-no-results"
        className="px-6 pt-8 flex flex-col gap-3 text-xs font-mono text-muted"
      >
        <p className="text-dim">{t("view.noResults")}</p>
        <p>{t("view.noResultsWhy")}</p>
        <ul className="flex flex-col gap-1 ps-4 list-disc">
          <li>{t("view.reasonNotIngested")}</li>
          <li>{t("view.reasonBeforeJoin")}</li>
          <li>{t("view.reasonNewDevice")}</li>
          <li>{t("view.reasonRetention")}</li>
          <li>{t("view.reasonDeleted")}</li>
        </ul>
      </div>
    );
  };

  const hasResults = hasMinLength && results.length > 0;

  return (
    <div data-testid="search-view" className="flex flex-col h-full bg-bg">
      {/* Search input */}
      <div className="px-4 py-3 flex-shrink-0 border-b border-line">
        <input
          data-testid="search-input"
          ref={inputRef}
          type="text"
          className="pollis-input font-mono"
          placeholder={t("view.placeholder")}
          value={inputValue}
          onChange={(e) => setInputValue(e.target.value)}
          autoComplete="off"
          spellCheck={false}
        />
        {inputValue.trim().length > 0 && inputValue.trim().length < 2 && (
          <p className="text-xs font-mono mt-1 text-muted">{t("view.minLength")}</p>
        )}
        <p className="text-[10px] font-mono mt-1 text-muted">{t("view.filterHint")}</p>
      </div>

      {/* Result count + sort toggle */}
      {hasResults && (
        <div className="px-4 py-2 flex-shrink-0 flex items-center justify-between gap-2 border-b border-line">
          <span data-testid="search-total" className="text-xs font-mono text-muted">
            {t("view.aboutResults", { count: total })}
          </span>
          <div className="flex items-center gap-1">
            <Button
              data-testid="search-sort-recent"
              variant={activeSort === "recent" ? "secondary" : "ghost"}
              size="xs"
              onClick={() => setSort("recent")}
            >
              {t("view.sortRecent")}
            </Button>
            <Button
              data-testid="search-sort-relevant"
              variant={activeSort === "relevant" ? "secondary" : "ghost"}
              size="xs"
              onClick={() => setSort("relevant")}
            >
              {t("view.sortRelevant")}
            </Button>
          </div>
        </div>
      )}

      {/* Results */}
      <div className="flex-1 overflow-y-auto">
        {hasResults ? (
          <>
            <ul>
              {results.map((result) => (
                <li key={result.message_id}>
                  <button
                    data-testid="search-result-item"
                    data-conversation-kind={result.conversation_kind ?? "unknown"}
                    onClick={() => onOpenResult(result)}
                    className="w-full text-start px-4 py-3 transition-colors bg-transparent hover:bg-hover border-b border-line"
                  >
                    {/* Sender and timestamp row */}
                    <div className="flex items-baseline justify-between gap-2 mb-1">
                      <span
                        data-testid="search-result-sender"
                        className="text-xs font-mono font-medium truncate text-accent"
                      >
                        {result.sender_username ?? result.sender_id}
                      </span>
                      <span className="text-xs font-mono flex-shrink-0 text-muted">
                        {formatShortDateTime(result.sent_at)}
                      </span>
                    </div>

                    {/* Where the message lives */}
                    <div
                      data-testid="search-result-conversation"
                      className="text-xs font-mono mb-1 truncate text-muted"
                    >
                      {conversationLabel(result)}
                      {result.thread_id ? ` · ${t("view.inThread")}` : ""}
                      {result.has_attachment ? ` · ${t("view.hasAttachment")}` : ""}
                    </div>

                    {/* Message snippet with highlight */}
                    <div className="text-xs font-mono text-dim">
                      <HighlightedSnippet snippet={result.snippet} />
                    </div>

                    <ResultThumbnails result={result} />
                  </button>
                </li>
              ))}
            </ul>
            {hasNextPage && (
              <div className="p-4 flex justify-center">
                <Button
                  data-testid="search-load-more"
                  variant="ghost"
                  size="sm"
                  onClick={() => void fetchNextPage()}
                  disabled={isFetchingNextPage}
                >
                  {isFetchingNextPage ? t("view.searching") : t("view.loadMore")}
                </Button>
              </div>
            )}
          </>
        ) : (
          renderEmptyState()
        )}
      </div>

      {/* What the corpus actually is. Persistent, because "no results" and
          "4,000 results" both mean something different depending on how much
          history this device holds. */}
      {corpus && (
        <CorpusFooter corpus={corpus} onOpenRetentionSettings={onOpenRetentionSettings} />
      )}
    </div>
  );
};

const CorpusFooter: React.FC<{
  corpus: SearchCorpus;
  onOpenRetentionSettings?: () => void;
}> = ({ corpus, onOpenRetentionSettings }) => {
  const { t } = useTranslation("search");
  const earliest = corpus.earliest_sent_at
    ? new Date(corpus.earliest_sent_at).toLocaleDateString()
    : null;

  return (
    <div
      data-testid="search-corpus-footer"
      className="px-4 py-2 flex-shrink-0 border-t border-line text-[10px] font-mono text-muted flex flex-wrap items-center gap-x-2 gap-y-1"
    >
      <span>
        {earliest
          ? t("view.corpusWithDate", { count: corpus.message_count, date: earliest })
          : t("view.corpus", { count: corpus.message_count })}
      </span>
      {corpus.indexing && <span data-testid="search-indexing">{t("view.indexing")}</span>}
      {corpus.retention_days > 0 && onOpenRetentionSettings && (
        <button
          type="button"
          onClick={onOpenRetentionSettings}
          className="underline bg-transparent text-accent"
        >
          {t("view.retentionLink", { days: corpus.retention_days })}
        </button>
      )}
      <button
        type="button"
        data-testid="search-learn-link"
        onClick={() => void shellOpen(LEARN_ON_DEVICE_SEARCH_URL)}
        className="underline bg-transparent text-accent"
      >
        {t("view.whyOnDevice")}
      </button>
    </div>
  );
};
