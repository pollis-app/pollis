import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Reply,
  Edit2,
  MoreHorizontal,
  MessagesSquare,
  Bookmark,
  Link,
  Check,
  AlertCircle,
  Trash2,
} from "lucide-react";

export type CopyLinkState = "idle" | "copied" | "failed";

interface MessageActionsProps {
  messageId: string;
  /** Which skin's toolbar chrome to render — floating panel (refined) or
   * inline row-end strip (terminal). The menu itself is identical. */
  variant: "refined" | "terminal";
  isOwn: boolean;
  canModerate: boolean;
  isSaved: boolean;
  copyLinkState: CopyLinkState;
  onReply?: (messageId: string) => void;
  onOpenThread?: (messageId: string) => void;
  onToggleSave?: (messageId: string) => void;
  onCopyLink?: (messageId: string) => void;
  onEdit?: (messageId: string) => void;
  onDelete?: (messageId: string) => void;
}

// Approximate rendered menu height plus margin — used to decide whether the
// menu opens upward or downward from the trigger.
const MENU_CLEARANCE_PX = 220;

/**
 * Per-message hover actions, Discord-shaped: Reply, Edit (own messages), and a
 * "more" trigger; every other action lives in an anchored menu with icon +
 * explicit label per row, Delete last.
 *
 * The menu is `absolute` inside the toolbar's own `relative` wrapper — anchored
 * to the trigger, no portal, no fixed positioning, no backdrop (the same
 * non-modal shape as EmojiPickerButton). Outside clicks close it via a
 * `mousedown` listener; Escape closes it too.
 */
export const MessageActions: React.FC<MessageActionsProps> = ({
  messageId,
  variant,
  isOwn,
  canModerate,
  isSaved,
  copyLinkState,
  onReply,
  onOpenThread,
  onToggleSave,
  onCopyLink,
  onEdit,
  onDelete,
}) => {
  const { t } = useTranslation("chat");
  const [menuOpen, setMenuOpen] = useState(false);
  // Whether the menu opens above the trigger — chosen at open time from the
  // trigger's viewport position, so the menu near the composer never renders
  // below the fold of the scrolling message list.
  const [opensUp, setOpensUp] = useState(true);
  const wrapperRef = useRef<HTMLDivElement>(null);
  const moreBtnRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!menuOpen) {
      return;
    }
    const handlePointerDown = (event: MouseEvent) => {
      if (wrapperRef.current && !wrapperRef.current.contains(event.target as Node)) {
        setMenuOpen(false);
      }
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        // Claim the key: the window-level nav.back shortcut also binds
        // Escape and would navigate the whole channel away while merely
        // closing this menu.
        event.stopImmediatePropagation();
        setMenuOpen(false);
      }
    };
    document.addEventListener("mousedown", handlePointerDown);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("mousedown", handlePointerDown);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [menuOpen]);

  const canDelete = (isOwn || canModerate) && !!onDelete;
  const hasMenu = !!onOpenThread || !!onToggleSave || !!onCopyLink || canDelete;

  const copyLinkIcon =
    copyLinkState === "copied" ? Check : copyLinkState === "failed" ? AlertCircle : Link;
  const copyLinkLabel =
    copyLinkState === "copied"
      ? t("actions.copyLinkCopied")
      : copyLinkState === "failed"
        ? t("actions.copyLinkFailed")
        : t("actions.copyLink");
  const copyLinkTone =
    copyLinkState === "copied"
      ? "text-[var(--c-text-accent)]"
      : copyLinkState === "failed"
        ? "text-danger"
        : "text-dim hover:text-fg";

  const toggleMenu = () => {
    if (!menuOpen) {
      const rect = moreBtnRef.current?.getBoundingClientRect();
      setOpensUp(!rect || window.innerHeight - rect.bottom < MENU_CLEARANCE_PX);
    }
    setMenuOpen((prev) => !prev);
  };

  const iconSize = variant === "refined" ? 16 : 18;
  // focus-visible mirrors hover so a bar button reached by arrow-key log
  // navigation shows the same affordance the pointer would.
  const barBtnClass =
    variant === "refined"
      ? "p-1 text-muted hover:text-[var(--c-text-accent)] focus-visible:text-[var(--c-text-accent)] outline-none"
      : "text-muted hover:text-[var(--c-text-accent)] focus-visible:text-[var(--c-text-accent)] outline-none";

  // The container owns hover-reveal for both skins; while the menu is open it
  // stays fully visible even if the pointer leaves the row — otherwise the
  // open menu would turn invisible yet keep intercepting clicks.
  // group-focus-within: the row root carries `group`, so keyboard focus on
  // the ROW (arrow-key log navigation) reveals the bar exactly like hover.
  const visibilityClass = menuOpen
    ? "opacity-100"
    : "opacity-0 group-hover:opacity-100 group-focus-within:opacity-100";
  const containerClass =
    variant === "refined"
      ? `absolute end-4 top-0 -translate-y-1/2 flex items-center gap-1 rounded-control border border-line bg-surface-raised px-1 py-0.5 transition-opacity ${visibilityClass}`
      : `flex-shrink-0 ms-2 flex items-center gap-4 h-6 ${visibilityClass}`;

  const menuRowClass =
    "flex w-full items-center gap-2 px-2.5 py-1.5 text-sm text-start whitespace-nowrap hover:bg-hover";

  const closeAnd = (action: () => void) => () => {
    setMenuOpen(false);
    action();
  };

  return (
    <div data-testid="message-actions" className={containerClass}>
      <button
        data-testid="reply-button"
        data-nav-action="reply"
        onClick={() => onReply?.(messageId)}
        aria-label={t("actions.reply")}
        className={barBtnClass}
      >
        <Reply size={iconSize} className="rtl-mirror" />
      </button>

      {isOwn && onEdit && (
        <button
          data-testid="edit-button"
          data-nav-action="edit"
          onClick={() => onEdit(messageId)}
          aria-label={t("actions.edit")}
          className={barBtnClass}
        >
          <Edit2 size={iconSize} />
        </button>
      )}

      {hasMenu && (
        <div ref={wrapperRef} className="relative flex items-center">
          <button
            ref={moreBtnRef}
            data-testid="message-actions-more"
            data-nav-action="more"
            onClick={toggleMenu}
            aria-label={t("actions.more")}
            aria-expanded={menuOpen}
            aria-haspopup="menu"
            className={barBtnClass}
            style={{ color: menuOpen ? "var(--c-text-accent)" : undefined }}
          >
            <MoreHorizontal size={iconSize} />
          </button>

          {menuOpen && (
            <div
              data-testid="message-actions-menu"
              role="menu"
              className={`absolute end-0 z-40 min-w-[11rem] rounded-control border border-line bg-surface-raised py-1 ${
                opensUp ? "bottom-full mb-1" : "top-full mt-1"
              }`}
            >
              {onOpenThread && (
                <button
                  role="menuitem"
                  data-testid="thread-button"
                  onClick={closeAnd(() => onOpenThread(messageId))}
                  className={`${menuRowClass} text-dim hover:text-fg`}
                >
                  <MessagesSquare size={16} className="shrink-0" />
                  {t("actions.replyInThread")}
                </button>
              )}
              {onToggleSave && (
                <button
                  role="menuitem"
                  data-testid="save-button"
                  onClick={closeAnd(() => onToggleSave(messageId))}
                  className={`${menuRowClass} text-dim hover:text-fg`}
                >
                  <Bookmark size={16} className="shrink-0" fill={isSaved ? "currentColor" : "none"} />
                  {isSaved ? t("actions.removeBookmark") : t("actions.save")}
                </button>
              )}
              {onCopyLink && (
                <button
                  role="menuitem"
                  data-testid="copy-link-button"
                  data-copy-state={copyLinkState}
                  // Stays open so the copied/failed feedback is visible where
                  // the click happened.
                  onClick={() => onCopyLink(messageId)}
                  className={`${menuRowClass} ${copyLinkTone}`}
                >
                  {React.createElement(copyLinkIcon, { size: 16, className: "shrink-0" })}
                  {copyLinkLabel}
                </button>
              )}
              {canDelete && onDelete && (
                <button
                  role="menuitem"
                  data-testid={isOwn ? "delete-button" : "admin-delete-button"}
                  onClick={closeAnd(() => onDelete(messageId))}
                  className={`${menuRowClass} text-danger`}
                >
                  <Trash2 size={16} className="shrink-0" />
                  {isOwn ? t("actions.delete") : t("actions.deleteAsAdmin")}
                </button>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
};
