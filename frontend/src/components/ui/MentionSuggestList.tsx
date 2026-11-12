import React from "react";
import { useTranslation } from "react-i18next";
import { SuggestList } from "./SuggestList";
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

  return (
    <SuggestList
      items={candidates}
      activeIndex={activeIndex}
      query={query}
      testId="mention-suggest-list"
      label={t("mentions.listLabel")}
      matchingKey="mentions.matching"
      itemKey={(c) => c.userId}
      optionTestId={(c) => `mention-option-${c.username}`}
      onSelect={onSelect}
      onHover={onHover}
      renderItem={(c) => (
        <>
          <Avatar avatarKey={c.avatarUrl ?? null} size={22} alt={c.username} />
          <span className="truncate text-fg">
            <MatchedName username={c.username} query={query} />
          </span>
          {c.displayName && c.displayName !== c.username && (
            <span className="truncate text-xs text-muted">{c.displayName}</span>
          )}
        </>
      )}
    />
  );
};
