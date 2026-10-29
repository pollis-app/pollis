import React, { useCallback } from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { appStore } from "../../../stores/appStore";
import { useShortcutLabel } from "../../../keyboard";
import { useRightPanel } from "./useRightPanel";
import { MembersPanel } from "./MembersPanel";
import { PinnedPanel } from "./PinnedPanel";
import { SafetyNumberPanel } from "./SafetyNumberPanel";
import { useDMConversations } from "../../../hooks/queries/useMessages";
import { ThreadPanel } from "./ThreadPanel";
import { ResizeHandle } from "../ResizeHandle";
import { usePanelWidth } from "../usePanelWidth";

/**
 * The right-hand context panel slot (#824): one slot switching on panel kind.
 * Whether it renders is device-local state navigation must not disturb (#904);
 * see `useRightPanel`. A flex column, never an overlay (no modals).
 */
export const RightPanel: React.FC = observer(() => {
  const { t } = useTranslation("nav");
  const { kind, isOpen, threadId, safetyPeerId, setPanel, setSafety } =
    useRightPanel();
  const { data: dmConversations = [] } = useDMConversations();
  const toggleRightPanelLabel = useShortcutLabel("app.toggleRightPanel");
  // Drag-collapse uses the same writer as the footer and shortcut, so there is
  // one closed state and reopening restores the dragged width.
  const closePanel = useCallback(() => setPanel("none"), [setPanel]);
  const { ref, widthClass, widthStyle, handleProps } = usePanelWidth(
    "rightPanel",
    closePanel,
  );

  const groupId = appStore.selectedGroupId;
  const channelId = appStore.selectedChannelId;
  const conversationId = appStore.selectedConversationId;
  // Context-free routes still show the panel; `MembersPanel` degrades to the
  // plain online roster there.
  const hasContext = Boolean(channelId || conversationId || groupId);
  // A stale `?thread=` or pins view on a context-free route falls back to the
  // roster rather than rendering empty.
  const shownThreadId = kind === "thread" && hasContext ? threadId : null;
  const showPins =
    kind === "pins" && hasContext && Boolean(channelId || conversationId);
  // "You & <name>" (#1563) only means something inside a DM with that person;
  // a stale `?safety=` anywhere else falls back to the roster.
  const safetyPeer =
    kind === "safety" && conversationId && safetyPeerId
      ? dmConversations
          .find((c) => c.id === conversationId)
          ?.peers?.find((p) => p.user_id === safetyPeerId) ?? null
      : null;

  if (!isOpen) {
    return null;
  }

  const label = shownThreadId
    ? t("panel.thread")
    : showPins
      ? t("pins.heading")
      : t("panel.details");
  // Two explicit keys rather than lowercasing `label` — casing rules are not
  // universal, and a translator needs the whole sentence either way.
  const closeLabel = shownThreadId
    ? t("panel.closeThread", { shortcut: toggleRightPanelLabel })
    : t("panel.closeDetails", { shortcut: toggleRightPanelLabel });

  return (
    <aside
      ref={ref}
      // `font-mono` is the skin switch: refined re-points
      // `.font-mono:not(.font-machine)` at the sans face.
      className={`relative flex ${widthClass} shrink-0 flex-col border-s border-line bg-surface font-mono`}
      style={widthStyle}
      data-testid="right-panel"
      aria-label={t("panel.ariaLabel")}
    >
      <div className="min-h-0 flex-1">
        {shownThreadId ? (
          <ThreadPanel
            threadId={shownThreadId}
            channelId={channelId}
            conversationId={conversationId}
          />
        ) : showPins ? (
          <PinnedPanel channelId={channelId} conversationId={conversationId} />
        ) : safetyPeer ? (
          <SafetyNumberPanel
            peerUserId={safetyPeer.user_id}
            peerLabel={`@${safetyPeer.username ?? safetyPeer.user_id}`}
            onBack={() => setSafety(undefined)}
          />
        ) : (
          <MembersPanel
            groupId={groupId}
            channelId={channelId}
            conversationId={conversationId}
          />
        )}
      </div>

      <button
        type="button"
        data-testid="right-panel-close"
        onClick={() => setPanel("none")}
        aria-label={closeLabel}
        title={closeLabel}
        className="flex shrink-0 cursor-pointer items-center gap-2 border-t border-line px-2.5 min-h-composer-flush text-start text-xs text-muted transition-colors hover:text-fg"
      >
        <span className="flex-1">{label}</span>
        <kbd
          aria-hidden="true"
          className="font-mono font-machine bg-bg px-1.5 py-px rounded-[3px] border border-line text-2xs leading-[1.2]"
        >
          {toggleRightPanelLabel}
        </kbd>
      </button>

      <ResizeHandle edge="start" label={t("panel.resize")} {...handleProps} />
    </aside>
  );
});
