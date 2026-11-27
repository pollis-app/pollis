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
import { PressButton } from "../ui/PressButton";
import { FOCUS_RING } from "../ui/focusRing";
import { useReactionBump, useReactionMotion } from "./useReactionMotion";
import { useLateArrival } from "../../hooks/useLateArrival";

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
  // Pills that arrive after the row painted fade in rather than blink (#2115).
  const arrived = useLateArrival(reactions.length > 0);
  // A pill joining the row pops in, a changed count rolls, a tapped pill bumps (#2125).
  const { pops, rolls } = useReactionMotion(reactions);
  const { bump, bumpClass } = useReactionBump();
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
      className={`relative flex items-center flex-wrap gap-1 ${arrived ? "motion-late-fade" : ""} ${className}`}
    >
      {reactions.map((reaction) => {
        const reacted = currentUser
          ? reaction.user_ids.includes(currentUser.id)
          : false;

        return (
          <PressButton
            key={reaction.emoji}
            activateOn="click"
            data-testid="reaction-pill"
            onClick={() => {
              bump(reaction.emoji);
              toggleReaction(reaction.emoji);
            }}
            onFocus={() => setHovered(true)}
            onContextMenu={(event) => {
              event.preventDefault();
              openPanel(reaction.emoji);
            }}
            title={tooltip(reaction.emoji, reaction.count)}
            className={`panel-raised flex items-center gap-1 px-1.5 py-0.5 text-secondary font-mono transition-colors duration-75 hover:opacity-90 ${FOCUS_RING} ${
              reacted ? "text-accent border-line-strong" : "text-muted"
            } ${pops(reaction.emoji) ? "motion-pop" : ""} ${bumpClass(reaction.emoji)}`}
            aria-label={t("reactions.pillLabel", {
              emoji: spokenEmoji(reaction.emoji),
              count: reaction.count,
            })}
            aria-description={t("reactions.contextHint")}
            aria-expanded={openEmoji === reaction.emoji}
            aria-pressed={reacted}
          >
            {/* A fixed slot: a custom image landing later never changes the pill's size (#2115). */}
            <span className="inline-flex h-5 min-w-5 items-center justify-center">
              <ReactionEmoji emoji={reaction.emoji} />
            </span>
            {/* A changed count rolls up into its slot (#2125); keyed so each change replays. */}
            <span className="motion-roll-slot">
              <span
                key={reaction.count}
                data-testid="reaction-count"
                className={rolls(reaction.emoji) ? "motion-roll" : "inline-block"}
              >
                {reaction.count}
              </span>
            </span>
          </PressButton>
        );
      })}
      {openEmoji !== null && tabs.length > 0 ? (
        <div
          data-placement={placement.placement}
          className={`absolute z-40 start-0 ${placement.placement === "up" ? "bottom-full mb-1 motion-popover-up" : "top-full mt-1 motion-popover-down"}`}
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
