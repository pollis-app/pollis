import React, { useCallback } from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { useRouterState } from "@tanstack/react-router";
import { useShortcutLabel } from "../../../keyboard";
import { useRightPanel } from "./useRightPanel";
import { MembersPanel } from "./MembersPanel";
import { PinnedPanel } from "./PinnedPanel";
import { SafetyNumberPanel } from "./SafetyNumberPanel";
import { useDMConversations } from "../../../hooks/queries/useMessages";
import { ThreadPanel } from "./ThreadPanel";
import { ResizeHandle } from "../ResizeHandle";
import { usePanelWidth } from "../usePanelWidth";
import { panelContextFromPath } from "./panelContext";
import { PanelCloseRow } from "../../ui/PanelCloseRow";
import { DMChat } from "../DMChat";

/** Right panel slot (#824): a flex sibling of `<Outlet />`, never an overlay. Open state is device-local (#904); see `useRightPanel`. */
export const RightPanel: React.FC = observer(() => {
  const { t } = useTranslation("nav");
  const { kind, isOpen, threadId, safetyPeerId, callDmId, setPanel, setSafety } =
    useRightPanel();
  const { data: dmConversations = [] } = useDMConversations();
  const toggleRightPanelLabel = useShortcutLabel("app.toggleRightPanel");
  // Drag-collapse uses the same writer as the footer and mod+shift+b.
  const closePanel = useCallback(() => setPanel("none"), [setPanel]);
  const { ref, widthClass, widthStyle, handleProps } = usePanelWidth(
    "rightPanel",
    closePanel,
  );

  // The page decides, never "last selected" store fields (`panelContext.ts`).
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const routeContext = panelContextFromPath(pathname);
  const { groupId, channelId } = routeContext;
  // On a 1:1 call the DM is the conversation, so its threads, pins and safety open here too (#2116).
  const conversationId = routeContext.conversationId ?? callDmId;
  const dmChatId = kind === "dm" ? callDmId : null;
  // No conversation: `MembersPanel` shows the online DM contacts.
  // Threads and pins need a message log (a channel or a DM); else the people list.
  const hasLog = Boolean(channelId || conversationId);
  const shownThreadId = kind === "thread" && hasLog ? threadId : null;
  const showPins = kind === "pins" && hasLog;
  // Safety (#1563) only inside a DM with that peer.
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
      : dmChatId
        ? t("panel.chat")
        : t("panel.details");
  // Two keys, not lowercased `label`: casing isn't universal.
  const closeLabel = shownThreadId
    ? t("panel.closeThread", { shortcut: toggleRightPanelLabel })
    : dmChatId
      ? t("panel.closeChat", { shortcut: toggleRightPanelLabel })
      : t("panel.closeDetails", { shortcut: toggleRightPanelLabel });

  return (
    <aside
      ref={ref}
      // `font-mono` is the skin switch; width is `--side-w` until dragged, never both.
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
        ) : dmChatId ? (
          // The real DM view, not a copy; no autofocus, so keys stay with the call.
          <div className="flex h-full flex-col" data-testid="right-panel-dm">
            <DMChat key={dmChatId} conversationId={dmChatId} autoFocus={false} />
          </div>
        ) : (
          <MembersPanel
            groupId={groupId}
            channelId={channelId}
            conversationId={conversationId}
          />
        )}
      </div>

      {/* Footer toggle, mirroring the left sidebar's. */}
      <PanelCloseRow
        data-testid="right-panel-close"
        activateOn="click"
        onClick={() => setPanel("none")}
        label={label}
        shortcut={toggleRightPanelLabel}
        ariaLabel={closeLabel}
      />

      <ResizeHandle edge="start" label={t("panel.resize")} {...handleProps} />
    </aside>
  );
});
