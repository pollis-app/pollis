import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useReactions, useToggleReaction } from "../../hooks/queries/useReactions";
import { useUserProfiles } from "../../hooks/queries/useUserProfile";
import { observer } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { activeLocale } from "../../utils/format";
import {
  buildReactionBreakdown,
  mergeProfiles,
  reactorIds,
  summarizeNames,
  type Reactor,
  type ReactorProfile,
} from "../../utils/reactionBreakdown";
import { type Placement, panelHeightPx, resolvePlacement } from "../Emoji/pickerPlacement";
import { ReactionEmoji, spokenEmoji } from "./ReactionEmoji";
import { ReactionBreakdownPanel } from "./ReactionBreakdownPanel";

interface MessageReactionsProps {
  messageId: string;
  className?: string;
}

/** ReactionBreakdownPanel's `max-h-[22rem]`. */
const PANEL_REM_HEIGHT = 22;

/**
 * Reaction pills; renders nothing until the message has one. Hovering a pill
 * names who reacted (its tooltip); right-clicking it (or the context-menu key)
 * opens "View reactions", the per-emoji breakdown. Names come from ONE batched
 * `get_user_profiles` read, made only once a pill is hovered or the panel opens.
 */
export const MessageReactions: React.FC<MessageReactionsProps> = observer(({ messageId, className = "" }) => {
  const { t } = useTranslation("chat");
  const { currentUser } = appStore;
  const { data: reactions = [] } = useReactions(messageId);
  const toggleReaction = useToggleReaction(messageId);
  const [hovered, setHovered] = useState(false);
  const [openEmoji, setOpenEmoji] = useState<string | null>(null);
  const [placement, setPlacement] = useState<Placement>({ placement: "up" });
  const wrapperRef = useRef<HTMLDivElement>(null);

  const ids = useMemo(() => reactorIds(reactions), [reactions]);
  const { data: fetched, isPending } = useUserProfiles(ids, hovered || openEmoji !== null);
  const selfId = currentUser?.id;
  const selfUsername = currentUser?.username;
  const profiles = useMemo(() => {
    const self = new Map<string, ReactorProfile>();
    if (selfId) {
      self.set(selfId, { username: selfUsername });
    }
    return mergeProfiles(self, fetched as ReadonlyMap<string, ReactorProfile> | undefined);
  }, [selfId, selfUsername, fetched]);
  const tabs = useMemo(
    () => buildReactionBreakdown(reactions, selfId, profiles),
    [reactions, selfId, profiles],
  );

  const close = useCallback(() => setOpenEmoji(null), []);

  // Outside clicks close it, like the emoji picker.
  useEffect(() => {
    if (openEmoji === null) {
      return;
    }
    const onPointerDown = (event: MouseEvent) => {
      if (wrapperRef.current && !wrapperRef.current.contains(event.target as Node)) {
        setOpenEmoji(null);
      }
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
  }, [openEmoji]);

  // Every reaction taken back while open: nothing left to show.
  useEffect(() => {
    if (openEmoji !== null && reactions.length === 0) {
      setOpenEmoji(null);
    }
  }, [openEmoji, reactions.length]);

  if (reactions.length === 0) {
    return null;
  }

  const nameOf = (person: Reactor) =>
    person.isSelf ? t("reactions.you") : (person.name ?? t("reactions.unknownUser"));

  const tooltip = (emoji: string, count: number) => {
    const tab = tabs.find((x) => x.key === emoji);
    const spoken = spokenEmoji(emoji);
    if (!tab || isPending) {
      return `${t("reactions.pillLabel", { emoji: spoken, count })}\n${t("reactions.contextHint")}`;
    }
    const { names, others } = summarizeNames(tab.people, nameOf);
    const list = new Intl.ListFormat(activeLocale(), { type: "conjunction" }).format(names);
    const line =
      others > 0
        ? t("reactions.hoverMore", { names: names.join(", "), count: others, emoji: spoken })
        : t("reactions.hoverNames", { names: list, emoji: spoken });
    return `${line}\n${t("reactions.contextHint")}`;
  };

  const openPanel = (emoji: string) => {
    const rect = wrapperRef.current?.getBoundingClientRect();
    if (rect) {
      setPlacement(
        resolvePlacement({
          preferred: "up",
          spaceAbove: rect.top,
          spaceBelow: window.innerHeight - rect.bottom,
          desired: panelHeightPx(PANEL_REM_HEIGHT),
        }),
      );
    }
    setOpenEmoji(emoji);
  };

  return (
    <div
      ref={wrapperRef}
      data-testid="message-reactions"
      onMouseEnter={() => setHovered(true)}
      className={`relative flex items-center flex-wrap gap-1 ${className}`}
    >
      {reactions.map((reaction) => {
        const reacted = currentUser
          ? reaction.user_ids.includes(currentUser.id)
          : false;

        return (
          <button
            key={reaction.emoji}
            data-testid="reaction-pill"
            onClick={() => toggleReaction(reaction.emoji)}
            onFocus={() => setHovered(true)}
            onContextMenu={(event) => {
              event.preventDefault();
              openPanel(reaction.emoji);
            }}
            title={tooltip(reaction.emoji, reaction.count)}
            className="panel-raised flex items-center gap-1 px-1.5 py-0.5 text-xs font-mono transition-colors duration-75 hover:opacity-90"
            style={{
              color: reacted ? "var(--c-accent)" : "var(--c-text-muted)",
              borderColor: reacted ? "var(--c-border-active)" : undefined,
            }}
            aria-label={t("reactions.pillLabel", {
              emoji: spokenEmoji(reaction.emoji),
              count: reaction.count,
            })}
            aria-description={t("reactions.contextHint")}
            aria-expanded={openEmoji === reaction.emoji}
            aria-pressed={reacted}
          >
            <ReactionEmoji emoji={reaction.emoji} />
            <span>{reaction.count}</span>
          </button>
        );
      })}
      {openEmoji !== null && tabs.length > 0 ? (
        <div
          data-placement={placement.placement}
          className={`absolute z-40 start-0 ${placement.placement === "up" ? "bottom-full mb-1" : "top-full mt-1"}`}
        >
          <ReactionBreakdownPanel
            key={openEmoji}
            tabs={tabs}
            initialEmoji={openEmoji}
            resolving={isPending}
            onClose={close}
          />
        </div>
      ) : null}
    </div>
  );
});
