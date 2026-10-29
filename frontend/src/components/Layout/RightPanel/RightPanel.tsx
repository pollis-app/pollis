import React, { useCallback } from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { appStore } from "../../../stores/appStore";
import { useShortcutLabel } from "../../../keyboard";
import { useRightPanel } from "./useRightPanel";
import { MembersPanel } from "./MembersPanel";
import { PinnedPanel } from "./PinnedPanel";
import { ThreadPanel } from "./ThreadPanel";
import { ResizeHandle } from "../ResizeHandle";
import { usePanelWidth } from "../usePanelWidth";

/**
 * The right-hand context panel slot (#824): switches on the panel kind.
 * Open/closed is device-local and navigation must not disturb it (#904); see
 * `useRightPanel` and `.codesight/wiki/ui.md` → "The right panel".
 */
export const RightPanel: React.FC = observer(() => {
  const { t } = useTranslation("nav");
  const { kind, isOpen, threadId, setPanel } = useRightPanel();
  const toggleRightPanelLabel = useShortcutLabel("app.toggleRightPanel");
  // Drag-collapse uses the same writer as the footer and `mod+shift+b`, so
  // reopening restores the dragged width.
  const closePanel = useCallback(() => setPanel("none"), [setPanel]);
  const { ref, widthClass, widthStyle, handleProps } = usePanelWidth(
    "rightPanel",
    closePanel,
  );

  const groupId = appStore.selectedGroupId;
  const channelId = appStore.selectedChannelId;
  const conversationId = appStore.selectedConversationId;
  // Routes without a conversation keep the column; `MembersPanel` degrades.
  const hasContext = Boolean(channelId || conversationId || groupId);
  // A thread or pins need a conversation; a stale `?thread=` shows the roster.
  const shownThreadId = kind === "thread" && hasContext ? threadId : null;
  const showPins =
    kind === "pins" && hasContext && Boolean(channelId || conversationId);

  if (!isOpen) {
    return null;
  }

  const label = shownThreadId
    ? t("panel.thread")
    : showPins
      ? t("pins.heading")
      : t("panel.details");
  // Two keys, not a lowercased `label`: casing rules aren't universal.
  const closeLabel = shownThreadId
    ? t("panel.closeThread", { shortcut: toggleRightPanelLabel })
    : t("panel.closeDetails", { shortcut: toggleRightPanelLabel });

  return (
    <aside
      ref={ref}
      // `font-mono` is the skin switch. Width is `--side-w` until dragged, then
      // px, never both. `relative` anchors the resize handle.
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
        ) : (
          <MembersPanel
            groupId={groupId}
            channelId={channelId}
            conversationId={conversationId}
          />
        )}
      </div>

      {/* Footer, mirroring the left sidebar's bottom-edge toggle. */}
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
