import React from "react";
import { observer } from "mobx-react-lite";
import { X } from "lucide-react";
import { appStore } from "../../../stores/appStore";
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

  return (
    <aside
      className="flex w-72 shrink-0 flex-col border-l border-line bg-surface"
      data-testid="right-panel"
      aria-label="Conversation details"
    >
      <div className="flex h-bar shrink-0 items-center justify-between border-b border-line px-3">
        <span className="text-xs font-medium uppercase tracking-widest text-dim">
          {kind === "thread" ? "Thread" : "Details"}
        </span>
        <button
          type="button"
          onClick={() => setPanel("none")}
          className="rounded p-1 text-muted hover:bg-hover hover:text-fg"
          aria-label="Close details panel"
          data-testid="right-panel-close"
        >
          <X size={14} aria-hidden />
        </button>
      </div>

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
    </aside>
  );
});
