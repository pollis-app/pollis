import React from "react";
import { observer } from "mobx-react-lite";
import { appStore } from "../../../stores/appStore";
import { useShortcutLabel } from "../../../keyboard";
import { useRightPanel } from "./useRightPanel";
import { MembersPanel } from "./MembersPanel";
import { ThreadPanel } from "./ThreadPanel";

/**
 * The right-hand context panel slot (#824).
 *
 * Deliberately a generic slot rather than a members-only sidebar: it switches
 * on the `panel` search param, so the thread panel (#825) lands as another
 * branch here instead of a second competing sidebar.
 *
 * Rendered as a flex sibling of `<Outlet />`, never a fixed overlay — the
 * project bans modals and backdrops, and a real column also means the message
 * list reflows instead of being covered.
 */
export const RightPanel: React.FC = observer(() => {
  const { kind, isOpen, threadId, setPanel } = useRightPanel();
  // Live label for the same command AppShell binds — rebinding the shortcut
  // in Preferences relabels this footer with no extra wiring.
  const toggleRightPanelLabel = useShortcutLabel("app.toggleRightPanel");

  const groupId = appStore.selectedGroupId;
  const channelId = appStore.selectedChannelId;
  const conversationId = appStore.selectedConversationId;
  // The panel is context for a conversation. On routes that have none
  // (preferences, search, the root page) there is nothing to show, so the
  // slot collapses rather than rendering an empty column.
  const hasContext = Boolean(channelId || conversationId || groupId);

  if (!isOpen || !hasContext) {
    return null;
  }

  const label = kind === "thread" ? "Thread" : "Details";

  return (
    <aside
      // `font-mono` is the whole skin switch: terminal renders it as DM Mono,
      // refined re-points `.font-mono:not(.font-machine)` at the sans face.
      // Width tracks `--side-w` so the panel mirrors the left sidebar's
      // per-skin measure instead of a fixed 18rem.
      className="flex w-[var(--side-w)] shrink-0 flex-col border-l border-line bg-surface font-mono"
      data-testid="right-panel"
      aria-label="Conversation details"
    >
      <div className="min-h-0 flex-1">
        {kind === "members" && (
          <MembersPanel
            groupId={groupId}
            channelId={channelId}
            conversationId={conversationId}
          />
        )}
        {/* `threadId` is non-null whenever kind is "thread" — the router drops
            the panel param rather than admit one without the other — but the
            guard keeps that guarantee local and typed. */}
        {kind === "thread" && threadId && (
          <ThreadPanel
            threadId={threadId}
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
        aria-label={`Close ${label.toLowerCase()} panel (${toggleRightPanelLabel})`}
        title={`Close ${label.toLowerCase()} panel (${toggleRightPanelLabel})`}
        className="flex shrink-0 cursor-pointer items-center gap-2 border-t border-line px-2.5 min-h-bar text-left text-xs text-muted transition-colors hover:text-fg"
      >
        <span className="flex-1">{label}</span>
        <kbd
          aria-hidden="true"
          className="font-mono font-machine bg-bg px-1.5 py-px rounded-[3px] border border-line text-2xs leading-[1.2]"
        >
          {toggleRightPanelLabel}
        </kbd>
      </button>
    </aside>
  );
});
