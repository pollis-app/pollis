import React, { useState, useEffect, useRef } from "react";
import { useSearchMessages } from "../../hooks/queries/useSearchMessages";
import { formatShortDateTime } from "../../utils/format";
import type { SearchResult } from "../../types";

// ─── Helpers ──────────────────────────────────────────────────────────────────

// Highlight occurrences of `term` in `text` by wrapping them in a <mark> span.
function HighlightedSnippet({ text, term }: { text: string; term: string }) {
  if (!term.trim()) {
    return <span>{text}</span>;
  }

  const escapedTerm = term.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const regex = new RegExp(`(${escapedTerm})`, "gi");
  const parts = text.split(regex);

  return (
    <span>
      {parts.map((part, i) => {
        if (regex.test(part)) {
          return (
            <mark
              key={i}
              style={{
                background: "var(--c-accent-muted)",
                color: "var(--c-accent-bright)",
                borderRadius: "2px",
                padding: "0 2px",
              }}
            >
              {part}
            </mark>
          );
        }
        return <span key={i}>{part}</span>;
      })}
    </span>
  );
}

// ─── Props ────────────────────────────────────────────────────────────────────

interface SearchViewProps {
  onNavigateToConversation: (conversationId: string) => void;
}

// ─── SearchView ───────────────────────────────────────────────────────────────

export const SearchView: React.FC<SearchViewProps> = ({ onNavigateToConversation }) => {
  const [inputValue, setInputValue] = useState("");
  const [debouncedQuery, setDebouncedQuery] = useState("");
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
  const { data: results, isFetching } = useSearchMessages(debouncedQuery);

  const handleResultClick = (result: SearchResult) => {
    onNavigateToConversation(result.conversation_id);
  };

  const renderEmptyState = () => {
    if (!debouncedQuery.trim() || !hasMinLength) {
      return (
        <p
          data-testid="search-empty-hint"
          className="text-xs font-mono text-center"
          style={{ color: "var(--c-text-muted)", paddingTop: "2rem" }}
        >
          Search your message history
        </p>
      );
    }

    if (isFetching) {
      return (
        <p
          className="text-xs font-mono text-center"
          style={{ color: "var(--c-text-muted)", paddingTop: "2rem" }}
        >
          Searching…
        </p>
      );
    }

    return (
      <p
        data-testid="search-no-results"
        className="text-xs font-mono text-center"
        style={{ color: "var(--c-text-muted)", paddingTop: "2rem" }}
      >
        No results
      </p>
    );
  };

  const hasResults = hasMinLength && !isFetching && results && results.length > 0;

  return (
    <div
      data-testid="search-view"
      className="flex flex-col h-full"
      style={{ background: "var(--c-bg)" }}
    >
      {/* Search input */}
      <div
        className="px-4 py-3 flex-shrink-0"
        style={{ borderBottom: "1px solid var(--c-border)" }}
      >
        <input
          data-testid="search-input"
          ref={inputRef}
          type="text"
          className="pollis-input font-mono"
          placeholder="Search messages…"
          value={inputValue}
          onChange={(e) => setInputValue(e.target.value)}
          autoComplete="off"
          spellCheck={false}
        />
        {inputValue.trim().length > 0 && inputValue.trim().length < 2 && (
          <p
            className="text-xs font-mono mt-1"
            style={{ color: "var(--c-text-muted)" }}
          >
            Type at least 2 characters to search
          </p>
        )}
      </div>

      {/* Results */}
      <div className="flex-1 overflow-y-auto">
        {hasResults ? (
          <ul>
            {results.map((result) => (
              <li key={result.message_id}>
                <button
                  data-testid="search-result-item"
                  onClick={() => handleResultClick(result)}
                  className="w-full text-left px-4 py-3 transition-colors bg-transparent hover:bg-[var(--c-hover)]"
                  style={{ borderBottom: "1px solid var(--c-border)" }}
                >
                  {/* Sender and timestamp row */}
                  <div className="flex items-baseline justify-between gap-2 mb-1">
                    <span
                      className="text-xs font-mono font-medium truncate"
                      style={{ color: "var(--c-accent)" }}
                    >
                      {result.sender_id}
                    </span>
                    <span
                      className="text-xs font-mono flex-shrink-0"
                      style={{ color: "var(--c-text-muted)" }}
                    >
                      {formatShortDateTime(result.sent_at)}
                    </span>
                  </div>

                  {/* Conversation ID (conversation context) */}
                  <div
                    className="text-xs font-mono mb-1 truncate"
                    style={{ color: "var(--c-text-muted)" }}
                  >
                    {result.conversation_id}
                  </div>

                  {/* Message snippet with highlight */}
                  <div
                    className="text-xs font-mono"
                    style={{ color: "var(--c-text-dim)" }}
                  >
                    <HighlightedSnippet text={result.snippet} term={debouncedQuery.trim()} />
                  </div>
                </button>
              </li>
            ))}
          </ul>
        ) : (
          renderEmptyState()
        )}
      </div>
    </div>
  );
};
