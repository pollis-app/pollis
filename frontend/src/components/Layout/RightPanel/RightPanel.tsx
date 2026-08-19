import React, { useCallback } from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { appStore } from "../../../stores/appStore";
import { useShortcutLabel } from "../../../keyboard";
import { useRightPanel } from "./useRightPanel";
import { MembersPanel } from "./MembersPanel";
import { ThreadPanel } from "./ThreadPanel";
import { ResizeHandle } from "../ResizeHandle";
import { usePanelWidth } from "../usePanelWidth";

/**
 * The right-hand context panel slot (#824).
 *
 * Deliberately a generic slot rather than a members-only sidebar: it switches
 * on the panel kind, so the thread panel (#825) lands as another branch here
 * instead of a second competing sidebar.
 *
 * Whether this renders at all is device-local state that navigation must not
 * disturb (#904); everything below the `isOpen` guard is the part that IS
 * route-reactive. See `useRightPanel`.
 *
 * Rendered as a flex sibling of `<Outlet />`, never a fixed overlay — the
 * project bans modals and backdrops, and a real column also means the message
 * list reflows instead of being covered.
 */
export const RightPanel: React.FC = observer(() => {
  const { t } = useTranslation("nav");
  const { kind, isOpen, threadId, setPanel } = useRightPanel();
  // Live label for the same command AppShell binds — rebinding the shortcut
  // in Preferences relabels this footer with no extra wiring.
  const toggleRightPanelLabel = useShortcutLabel("app.toggleRightPanel");
  // Collapsing by drag goes through `setPanel("none")` — the same writer the
  // footer button and `mod+shift+b` use, so the panel has exactly one closed
  // state and reopening restores the width it was dragged from.
  const closePanel = useCallback(() => setPanel("none"), [setPanel]);
  const { ref, widthClass, widthStyle, handleProps } = usePanelWidth(
    "rightPanel",
    closePanel,
  );

  const groupId = appStore.selectedGroupId;
  const channelId = appStore.selectedChannelId;
  const conversationId = appStore.selectedConversationId;
  // Routes like Preferences, Search and the root page have no conversation.
  // The panel used to collapse entirely there; now only its CONTENT is
  // contextual — `MembersPanel` degrades to the plain online roster and drops
  // the media grid, because "who is online" is useful on every route while
  // "media shared in this conversation" is not.
  const hasContext = Boolean(channelId || conversationId || groupId);
  // A thread is meaningless without the conversation it hangs off, so a
  // stale `?thread=` on a context-free route falls back to the roster
  // rather than rendering an empty thread.
  const shownThreadId = kind === "thread" && hasContext ? threadId : null;

  if (!isOpen) {
    return null;
  }

  const label = shownThreadId ? t("panel.thread") : t("panel.details");
  // Two explicit keys rather than lowercasing `label` — casing rules are not
  // universal, and a translator needs the whole sentence either way.
  const closeLabel = shownThreadId
    ? t("panel.closeThread", { shortcut: toggleRightPanelLabel })
    : t("panel.closeDetails", { shortcut: toggleRightPanelLabel });

  return (
    <aside
      ref={ref}
      // `font-mono` is the whole skin switch: terminal renders it as DM Mono,
      // refined re-points `.font-mono:not(.font-machine)` at the sans face.
      // Width tracks `--side-w` so the panel mirrors the left sidebar's
      // per-skin measure instead of a fixed 18rem, until the user drags it —
      // then it is a dragged pixel value instead, never both. `relative`
      // anchors the resize handle to this edge.
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
        ) : (
          <MembersPanel
            groupId={groupId}
            channelId={channelId}
            conversationId={conversationId}
          />
        )}
      </div>

      {/* Footer, not a header: the same affordance the left sidebar puts at
          its bottom edge, mirrored to this one. Label on the left, live key
          combo on the right, whole strip is the toggle. */}
      <button
        type="button"
        data-testid="right-panel-close"
        onClick={() => setPanel("none")}
        aria-label={closeLabel}
        title={closeLabel}
        className="flex shrink-0 cursor-pointer items-center gap-2 border-t border-line px-2.5 min-h-bar text-start text-xs text-muted transition-colors hover:text-fg"
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
