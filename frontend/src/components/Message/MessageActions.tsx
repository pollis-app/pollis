import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Reply,
  Edit2,
  MoreHorizontal,
  MessagesSquare,
  Bookmark,
  Pin,
  PinOff,
  Link,
  Check,
  AlertCircle,
  Trash2,
  Flag,
} from "lucide-react";
import { ReactionAddButton } from "./ReactionAddButton";
import { PressButton } from "../ui/PressButton";

export type CopyLinkState = "idle" | "copied" | "failed";

interface MessageActionsProps {
  messageId: string;
  /** Toolbar chrome: floating panel (refined) or inline strip (terminal). */
  variant: "refined" | "terminal";
  isOwn: boolean;
  canModerate: boolean;
  isSaved: boolean;
  isPinned: boolean;
  copyLinkState: CopyLinkState;
  onReply?: (messageId: string) => void;
  onOpenThread?: (messageId: string) => void;
  onToggleSave?: (messageId: string) => void;
  onTogglePin?: (messageId: string) => void;
  onCopyLink?: (messageId: string) => void;
  onEdit?: (messageId: string) => void;
  onDelete?: (messageId: string) => void;
  onReport?: (messageId: string) => void;
}

const MENU_CLEARANCE_PX = 220;

/** Per-message hover actions plus an anchored "more" menu (no portal, no backdrop). */
export const MessageActions: React.FC<MessageActionsProps> = ({
  messageId,
  variant,
  isOwn,
  canModerate,
  isSaved,
  isPinned,
  copyLinkState,
  onReply,
  onOpenThread,
  onToggleSave,
  onTogglePin,
  onCopyLink,
  onEdit,
  onDelete,
  onReport,
}) => {
  const { t } = useTranslation("chat");
  const [menuOpen, setMenuOpen] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);
  // Opens up or down from the trigger's viewport position, chosen at open time.
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
        // Claim the key: window-level nav.back also binds Escape.
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
  const canReport = !isOwn && !!onReport;
  const hasMenu =
    !!onOpenThread || !!onToggleSave || !!onTogglePin || !!onCopyLink || canDelete || canReport;

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
      ? "text-accent"
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
  // Plain `focus:` (not focus-visible): programmatic focus isn't focus-visible on WebKitGTK. One `text-*` per button.
  const barBtnFlipClass = "outline-none focus:bg-accent focus:text-bg";
  const barBtnBaseClass =
    variant === "refined"
      ? `p-1 rounded-control ${barBtnFlipClass}`
      : `p-0.5 ${barBtnFlipClass}`;
  const barBtnClass = `${barBtnBaseClass} text-muted hover:text-accent`;

  // Visible while a menu is open; hidden bars must not take the pointer (they overlap the row above).
  const visibilityClass = menuOpen || pickerOpen
    ? "opacity-100"
    : "opacity-0 pointer-events-none group-hover:opacity-100 group-hover:pointer-events-auto group-focus-within:opacity-100 group-focus-within:pointer-events-auto";
  const containerClass =
    variant === "refined"
      ? `absolute end-4 top-0 -translate-y-1/2 flex items-center gap-1 rounded-control border border-line bg-surface-raised px-1 py-0.5 transition-opacity ${visibilityClass}`
      : `flex-shrink-0 ms-2 flex items-center gap-4 h-6 ${visibilityClass}`;

  const menuRowClass =
    "flex w-full items-center gap-2 px-2.5 py-1.5 text-sm text-start whitespace-nowrap hover:bg-hover outline-none";
  // Appended per row: two `focus:bg-*` on one element resolve by stylesheet order.
  const menuRowFocusClass = "focus:bg-accent focus:text-bg";
  const menuRowFocusDangerClass = "focus:bg-danger focus:text-bg";
  // Copied row keeps focus, so it steps down to `bg-active` to show its tone (#1059).
  const copyLinkFocusClass =
    copyLinkState === "copied"
      ? "focus:bg-active focus:text-accent"
      : copyLinkState === "failed"
        ? menuRowFocusDangerClass
        : menuRowFocusClass;

  const closeAnd = (action: () => void) => () => {
    setMenuOpen(false);
    action();
  };

  return (
    <div data-testid="message-actions" className={containerClass}>
      <PressButton
        data-testid="reply-button"
        data-nav-action="reply"
        onClick={() => onReply?.(messageId)}
        aria-label={t("actions.reply")}
        className={barBtnClass}
      >
        <Reply size={iconSize} className="rtl-mirror" />
      </PressButton>

      <ReactionAddButton
        messageId={messageId}
        iconSize={iconSize}
        className={barBtnClass}
        onOpenChange={setPickerOpen}
      />

      {isOwn && onEdit && (
        <PressButton
          data-testid="edit-button"
          data-nav-action="edit"
          onClick={() => onEdit(messageId)}
          aria-label={t("actions.edit")}
          className={barBtnClass}
        >
          <Edit2 size={iconSize} />
        </PressButton>
      )}

      {hasMenu && (
        <div ref={wrapperRef} className="relative flex items-center">
          <PressButton
            ref={moreBtnRef}
            data-testid="message-actions-more"
            data-nav-action="more"
            onClick={toggleMenu}
            aria-label={t("actions.more")}
            aria-expanded={menuOpen}
            aria-haspopup="menu"
            className={`${barBtnBaseClass} ${
              menuOpen ? "text-accent" : "text-dim hover:text-accent"
            }`}
          >
            <MoreHorizontal size={iconSize} />
          </PressButton>

          {menuOpen && (
            <div
              data-testid="message-actions-menu"
              role="menu"
              className={`absolute end-0 z-40 min-w-[11rem] rounded-control border border-line bg-surface-raised py-1 ${
                opensUp ? "bottom-full mb-1" : "top-full mt-1"
              }`}
            >
              {onOpenThread && (
                <PressButton
                  role="menuitem"
                  data-testid="thread-button"
                  onClick={closeAnd(() => onOpenThread(messageId))}
                  className={`${menuRowClass} ${menuRowFocusClass} text-dim hover:text-fg`}
                >
                  <MessagesSquare size={16} className="shrink-0" />
                  {t("actions.replyInThread")}
                </PressButton>
              )}
              {onToggleSave && (
                <button
                  role="menuitem"
                  data-testid="save-button"
                  onClick={closeAnd(() => onToggleSave(messageId))}
                  className={`${menuRowClass} ${menuRowFocusClass} text-dim hover:text-fg`}
                >
                  <Bookmark size={16} className="shrink-0" fill={isSaved ? "currentColor" : "none"} />
                  {isSaved ? t("actions.removeBookmark") : t("actions.save")}
                </button>
              )}
              {onTogglePin && (
                <button
                  role="menuitem"
                  data-testid="pin-button"
                  onClick={closeAnd(() => onTogglePin(messageId))}
                  className={`${menuRowClass} ${menuRowFocusClass} text-dim hover:text-fg`}
                >
                  {isPinned ? (
                    <PinOff size={16} className="shrink-0" />
                  ) : (
                    <Pin size={16} className="shrink-0" />
                  )}
                  {isPinned ? t("actions.unpin") : t("actions.pin")}
                </button>
              )}
              {onCopyLink && (
                <button
                  role="menuitem"
                  data-testid="copy-link-button"
                  data-copy-state={copyLinkState}
                  onClick={() => onCopyLink(messageId)}
                  className={`${menuRowClass} ${copyLinkFocusClass} ${copyLinkTone}`}
                >
                  {React.createElement(copyLinkIcon, { size: 16, className: "shrink-0" })}
                  {copyLinkLabel}
                </button>
              )}
              {canReport && onReport && (
                <button
                  role="menuitem"
                  data-testid="report-button"
                  onClick={closeAnd(() => onReport(messageId))}
                  className={`${menuRowClass} ${menuRowFocusDangerClass} text-danger`}
                >
                  <Flag size={16} className="shrink-0" />
                  {t("actions.report")}
                </button>
              )}
              {canDelete && onDelete && (
                <button
                  role="menuitem"
                  data-testid={isOwn ? "delete-button" : "admin-delete-button"}
                  onClick={closeAnd(() => onDelete(messageId))}
                  className={`${menuRowClass} ${menuRowFocusDangerClass} text-danger`}
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
