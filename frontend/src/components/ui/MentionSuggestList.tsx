import React, { useEffect, useRef } from "react";
import { Avatar } from "./Avatar";
import type { MentionCandidate } from "../../utils/mentions";

interface MentionSuggestListProps {
  candidates: MentionCandidate[];
  /** Index of the highlighted row — moved by the arrow keys in ChatInput. */
  activeIndex: number;
  /** What the user has typed after the '@', used to bold the matched prefix. */
  query: string;
  onSelect: (candidate: MentionCandidate) => void;
  onHover: (index: number) => void;
}

/**
 * Highlight the part of `username` that matches what the user has typed —
 * the small touch that makes Slack's list feel responsive to each keystroke.
 */
const MatchedName: React.FC<{ username: string; query: string }> = ({
  username,
  query,
}) => {
  const at = query ? username.toLowerCase().indexOf(query.toLowerCase()) : -1;
  if (at < 0) {
    return <>{username}</>;
  }
  return (
    <>
      {username.slice(0, at)}
      <span className="font-semibold text-accent">
        {username.slice(at, at + query.length)}
      </span>
      {username.slice(at + query.length)}
    </>
  );
};

/**
 * Refined skin's mention autocomplete (#843) — the Slack/Discord inline list
 * that sits directly above the composer. Arrow keys move, Enter/Tab accept,
 * Esc dismisses; all of that is driven by ChatInput's key handler, which owns
 * the textarea.
 *
 * Positioning is `absolute` INSIDE the composer's own relative container
 * (`bottom-full`), not a portal to `document.body` and not `position: fixed`.
 * It is a normal part of the composer's layout that happens to overhang the
 * message list, which is exactly what Slack does and what the no-modals rule
 * allows.
 */
export const MentionSuggestList: React.FC<MentionSuggestListProps> = ({
  candidates,
  activeIndex,
  query,
  onSelect,
  onHover,
}) => {
  const activeRef = useRef<HTMLButtonElement>(null);

  // Keep the highlighted row in view when the arrow keys walk past the edge
  // of the scroll box.
  useEffect(() => {
    activeRef.current?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  if (candidates.length === 0) {
    return null;
  }

  return (
    <div
      data-testid="mention-suggest-list"
      role="listbox"
      aria-label="Mention suggestions"
      className="absolute bottom-full left-0 right-0 z-20 mb-1 mx-2 overflow-hidden rounded-panel border border-line bg-surface-raised"
    >
      <div className="px-3 py-1.5 border-b border-line text-2xs text-muted select-none">
        Members matching <span className="text-fg">@{query}</span>
      </div>
      {/* Caps the list at roughly six rows (max-h-60 is 15rem, so it tracks
          the user's font setting rather than a pixel height). */}
      <div className="max-h-60 overflow-y-auto py-1">
        {candidates.map((c, i) => {
          const isActive = i === activeIndex;
          return (
            <button
              key={c.userId}
              ref={isActive ? activeRef : undefined}
              type="button"
              role="option"
              aria-selected={isActive}
              data-testid={`mention-option-${c.username}`}
              data-active={isActive ? "true" : undefined}
              // The composer must keep focus — a blur would tear down the
              // list before the click ever lands.
              onMouseDown={(e) => {
                e.preventDefault();
                onSelect(c);
              }}
              onMouseEnter={() => onHover(i)}
              className={`w-full flex items-center gap-2 px-3 py-1.5 text-left text-sm hover:bg-hover ${
                isActive ? "bg-active" : ""
              }`}
            >
              <Avatar avatarKey={c.avatarUrl ?? null} size={22} alt={c.username} />
              <span className="truncate text-fg">
                <MatchedName username={c.username} query={query} />
              </span>
              {c.displayName && c.displayName !== c.username && (
                <span className="truncate text-xs text-muted">{c.displayName}</span>
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
};
