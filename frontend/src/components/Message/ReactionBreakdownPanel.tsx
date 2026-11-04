import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { Avatar } from "../ui/Avatar";
import { ReactionEmoji, spokenEmoji } from "./ReactionEmoji";
import {
  selectTab,
  type Reactor,
  type ReactionTab,
} from "../../utils/reactionBreakdown";

interface ReactionBreakdownPanelProps {
  tabs: readonly ReactionTab[];
  initialEmoji: string;
  /** The batched profile read is still out: unnamed rows wait instead of saying "Unknown user". */
  resolving: boolean;
  onClose: () => void;
}

/**
 * "View reactions" (Discord's right-click on a pill): a tab per emoji, All first
 * when there are several, the clicked one selected, people in reaction order.
 * Anchored `absolute` by its host like the emoji picker — no portal, no backdrop.
 */
export const ReactionBreakdownPanel: React.FC<ReactionBreakdownPanelProps> = ({
  tabs,
  initialEmoji,
  resolving,
  onClose,
}) => {
  const { t } = useTranslation("chat");
  const navigate = useNavigate();
  const [wanted, setWanted] = useState(initialEmoji);
  const tab = selectTab(tabs, wanted);
  const tabRefs = useRef<Map<string, HTMLButtonElement>>(new Map());

  // Keyboard users land on the selected tab.
  useEffect(() => {
    if (tab) {
      tabRefs.current.get(tab.key)?.focus();
    }
    // Only on open.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onTabKey = (event: React.KeyboardEvent) => {
    if (!tab) {
      return;
    }
    const index = tabs.findIndex((x) => x.key === tab.key);
    const rtl = document.documentElement.dir === "rtl";
    const step =
      event.key === "ArrowRight" ? (rtl ? -1 : 1) : event.key === "ArrowLeft" ? (rtl ? 1 : -1) : 0;
    if (step === 0) {
      return;
    }
    event.preventDefault();
    const next = tabs[(index + step + tabs.length) % tabs.length];
    setWanted(next.key);
    tabRefs.current.get(next.key)?.focus();
  };

  return (
    <div
      data-testid="reaction-breakdown"
      role="group"
      aria-label={t("reactions.sheetTitle")}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          onClose();
        }
      }}
      className="panel-raised flex flex-col overflow-hidden w-[20rem] max-h-[22rem]"
    >
      <div
        role="tablist"
        aria-label={t("reactions.sheetTitle")}
        onKeyDown={onTabKey}
        className="shrink-0 flex items-center gap-1 overflow-x-auto p-1.5 border-b border-line"
      >
        {tabs.map((x) => {
          const on = x.key === tab?.key;
          return (
            <button
              key={x.key}
              ref={(el) => {
                if (el) {
                  tabRefs.current.set(x.key, el);
                } else {
                  tabRefs.current.delete(x.key);
                }
              }}
              type="button"
              role="tab"
              data-testid={`reaction-tab-${x.emoji === null ? "all" : x.emoji}`}
              aria-selected={on}
              tabIndex={on ? 0 : -1}
              aria-label={
                x.emoji === null
                  ? t("reactions.allTabLabel", { count: x.count })
                  : t("reactions.tabLabel", { emoji: spokenEmoji(x.emoji), count: x.count })
              }
              onClick={() => setWanted(x.key)}
              className={`shrink-0 flex items-center gap-1 px-2 py-1 rounded-chip text-xs font-mono transition-colors duration-75 ${
                on ? "bg-surface-high text-accent font-bold" : "text-muted hover:bg-hover"
              }`}
            >
              {x.emoji === null ? <span>{t("reactions.all")}</span> : <ReactionEmoji emoji={x.emoji} />}
              <span>{x.count}</span>
            </button>
          );
        })}
      </div>
      {tab ? (
        <ul role="tabpanel" data-testid="reaction-breakdown-list" className="flex-1 min-h-0 overflow-y-auto py-1">
          {tab.people.map((person) => (
            <ReactorRow
              key={person.userId}
              person={person}
              tabEmoji={tab.emoji}
              resolving={resolving}
              onOpen={
                person.isSelf || !person.name
                  ? undefined
                  : () => {
                      onClose();
                      navigate({ to: "/user/$userId", params: { userId: person.userId } });
                    }
              }
            />
          ))}
        </ul>
      ) : null}
    </div>
  );
};

const ReactorRow: React.FC<{
  person: Reactor;
  /** Null on the All tab, which also draws each person's emojis. */
  tabEmoji: string | null;
  resolving: boolean;
  onOpen?: () => void;
}> = ({ person, tabEmoji, resolving, onOpen }) => {
  const showEmojis = tabEmoji === null;
  const { t } = useTranslation("chat");
  // Never the raw id.
  const name = person.name ?? (resolving ? "…" : t("reactions.unknownUser"));
  const spokenName = person.isSelf ? t("reactions.selfName", { name }) : name;
  const label = t("reactions.rowLabel", {
    name: person.handle ? `${spokenName}, @${person.handle}` : spokenName,
    emojis: (showEmojis ? person.emojis : [tabEmoji]).map(spokenEmoji).join(" "),
  });
  const body = (
    <>
      <Avatar avatarKey={person.avatarKey ?? null} size={24} alt="" />
      <span className="flex-1 min-w-0 flex items-baseline gap-1.5">
        <span className="truncate text-sm text-fg">{name}</span>
        {person.handle ? (
          <span className="truncate text-xs font-mono text-muted">@{person.handle}</span>
        ) : null}
      </span>
      {person.isSelf ? (
        <span className="shrink-0 px-1.5 rounded-chip bg-surface-high text-accent text-2xs font-medium">
          {t("reactions.you")}
        </span>
      ) : null}
      {showEmojis ? (
        <span className="shrink-0 flex items-center gap-0.5" aria-hidden>
          {person.emojis.map((emoji) => (
            <ReactionEmoji key={emoji} emoji={emoji} />
          ))}
        </span>
      ) : null}
    </>
  );
  const rowClass = "w-full flex items-center gap-2 px-2.5 py-1.5 text-start";
  return (
    <li data-testid="reaction-breakdown-row" data-user-id={person.userId}>
      {onOpen ? (
        <button type="button" onClick={onOpen} aria-label={label} className={`${rowClass} hover:bg-hover`}>
          {body}
        </button>
      ) : (
        <div aria-label={label} className={rowClass}>
          {body}
        </div>
      )}
    </li>
  );
};
