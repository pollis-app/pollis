import React, { useEffect, useRef } from "react";
import { Trans, useTranslation } from "react-i18next";
import { Avatar } from "./Avatar";
import type { MentionCandidate } from "../../utils/mentions";

interface MentionSuggestListProps {
  candidates: MentionCandidate[];
  activeIndex: number;
  query: string;
  onSelect: (candidate: MentionCandidate) => void;
  onHover: (index: number) => void;
}

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

/** Refined skin's mention list (#843), `absolute` inside the composer (no portal); ChatInput owns the keys. */
export const MentionSuggestList: React.FC<MentionSuggestListProps> = ({
  candidates,
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

  if (candidates.length === 0) {
    return null;
  }

  return (
    <div
      data-testid="mention-suggest-list"
      role="listbox"
      aria-label={t("mentions.listLabel")}
      className="absolute bottom-full left-0 right-0 z-20 mb-1 mx-2 overflow-hidden rounded-panel border border-line bg-surface-raised"
    >
      <div className="px-3 py-1.5 border-b border-line text-2xs text-muted select-none">
        {/* `Trans`, so a translator can move the highlighted query. */}
        <Trans
          i18nKey="mentions.matching"
          ns="common"
          values={{ query }}
          components={{ hl: <span className="text-fg" /> }}
        />
      </div>
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
              // Keep composer focus: a blur tears down the list first.
              onMouseDown={(e) => {
                e.preventDefault();
                onSelect(c);
              }}
              onMouseEnter={() => onHover(i)}
              className={`w-full flex items-center gap-2 px-3 py-1.5 text-start text-sm hover:bg-hover ${
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
