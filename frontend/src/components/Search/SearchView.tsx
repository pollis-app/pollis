import React, { useState, useEffect, useRef, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useSearchMessages } from "../../hooks/queries/useSearchMessages";
import { formatShortDateTime } from "../../utils/format";
import { Button } from "../ui/Button";
import { TextInput } from "../ui/TextInput";
import { EmojiText } from "../Emoji/EmojiText";
import { ResultThumbnails } from "./ResultThumbnails";
import { shellOpen } from "../../bridge";
import type { SearchCorpus, SearchResult, SearchSnippet, SearchSort } from "../../types";
import { PressButton } from "../ui/PressButton";

/** `/learn` entry on why search is on-device: the server holds ciphertext and can't search. */
const LEARN_ON_DEVICE_SEARCH_URL = "https://pollis.com/learn#on-device-search";

/**
 * Render a snippet by slicing at the `[start, end)` UTF-16 ranges Rust
 * computed: no regex, and no `dangerouslySetInnerHTML`, so a body can never
 * become markup. Slices go through `EmojiText`; a range that cuts a token
 * leaves that fragment literal.
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

/** `#general` in Acme, `@bob`, or the bare id when this device never listed it. */
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

interface SearchViewProps {
  onOpenResult: (result: SearchResult) => void;
  onOpenRetentionSettings?: () => void;
  initialQuery?: string;
}

export const SearchView: React.FC<SearchViewProps> = ({
  onOpenResult,
  onOpenRetentionSettings,
  initialQuery = "",
}) => {
  const { t } = useTranslation("search");
  const [inputValue, setInputValue] = useState(initialQuery);
  // Seeded, not debounced: a handed-off query is already what the user meant.
  const [debouncedQuery, setDebouncedQuery] = useState(initialQuery);
  // Null lets the backend decide: relevance globally, recency in a conversation.
  const [sort, setSort] = useState<SearchSort | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

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
  // What the backend applied, shown when the user hasn't overridden it.
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

    // Not "No results": a message can be missing for reasons inherent to E2EE,
    // not the query (#850 §6).
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
      <div className="px-4 py-3 flex-shrink-0 border-b border-line">
        <TextInput
          data-testid="search-input"
          ariaLabel={t("view.placeholder")}
          placeholder={t("view.placeholder")}
          value={inputValue}
          onChange={setInputValue}
          autoFocus
        />
        {inputValue.trim().length > 0 && inputValue.trim().length < 2 && (
          <p className="text-xs font-mono mt-1 text-muted">{t("view.minLength")}</p>
        )}
        <p className="text-[10px] font-mono mt-1 text-muted">{t("view.filterHint")}</p>
      </div>

      {hasResults && (
        <div className="px-4 py-2 flex-shrink-0 flex items-center justify-between gap-2 border-b border-line">
          <span data-testid="search-total" className="text-xs font-mono text-muted">
            {t("view.aboutResults", { count: total })}
          </span>
          <div className="flex items-center gap-1">
            <Button
              data-testid="search-sort-recent"
              activateOn="press"
              variant={activeSort === "recent" ? "secondary" : "ghost"}
              size="xs"
              onClick={() => setSort("recent")}
            >
              {t("view.sortRecent")}
            </Button>
            <Button
              data-testid="search-sort-relevant"
              activateOn="press"
              variant={activeSort === "relevant" ? "secondary" : "ghost"}
              size="xs"
              onClick={() => setSort("relevant")}
            >
              {t("view.sortRelevant")}
            </Button>
          </div>
        </div>
      )}

      <div className="flex-1 overflow-y-auto">
        {hasResults ? (
          <>
            <ul>
              {results.map((result) => (
                <li key={result.message_id}>
                  <PressButton
                    data-testid="search-result-item"
                    data-conversation-kind={result.conversation_kind ?? "unknown"}
                    onClick={() => onOpenResult(result)}
                    className="w-full text-start px-4 py-3 bg-transparent hover:bg-hover border-b border-line"
                  >
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

                    <div
                      data-testid="search-result-conversation"
                      className="text-xs font-mono mb-1 truncate text-muted"
                    >
                      {conversationLabel(result)}
                      {result.thread_id ? ` · ${t("view.inThread")}` : ""}
                      {result.has_attachment ? ` · ${t("view.hasAttachment")}` : ""}
                    </div>

                    <div className="text-xs font-mono text-dim" data-testid="search-result-snippet">
                      <HighlightedSnippet snippet={result.snippet} />
                    </div>

                    <ResultThumbnails result={result} />
                  </PressButton>
                </li>
              ))}
            </ul>
            {hasNextPage && (
              <div className="p-4 flex justify-center">
                <Button
                  data-testid="search-load-more"
                  activateOn="press"
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

      {/* Persistent: result counts only mean something next to how much history this device holds. */}
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
