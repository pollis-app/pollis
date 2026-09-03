import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import {
  Image as ImageIcon,
  MessageSquare,
  Pencil,
  Pin,
  Search,
  Trash2,
  Vault as VaultIcon,
  X,
} from "lucide-react";
import { ChatInput, type ChatInputHandle, type Attachment } from "../components/ui/ChatInput";
import { Button } from "../components/ui/Button";
import { EditMessageBar } from "../components/ui/EditMessageBar";
import { LinkifiedText } from "../components/ui/LinkifiedText";
import { AttachmentDisplay } from "../components/Message/AttachmentDisplay";
import { MediaGalleryView } from "../components/Media/MediaGalleryView";
import { parseContent } from "../hooks/queries/useMessages";
import {
  useDeleteVaultMessage,
  useEditVaultMessage,
  useSendVaultMessage,
  useSetVaultMessagePinned,
  useVaultMessages,
  type VaultMessage,
} from "../hooks/queries/useVault";
import { buildMessageContent } from "../utils/attachmentEnvelope";
import { formatClockTime, formatDayDivider } from "../utils/format";
import type { MessageAttachment } from "../types";

type VaultView = "chat" | "media";

/**
 * The Vault (#107): a private, end-to-end-encrypted space that works like a
 * chat with yourself — Signal's Note to Self, with Drive semantics. Entries
 * sync across every enrolled device (the account key decrypts them, so a new
 * device starts FULL here, not empty), files ride the normal attachment
 * pipeline, and the media toggle flips the same content into a
 * Photos-style roll (`MediaGalleryView`, deliberately generic so channels
 * and DMs can adopt it later).
 *
 * Confirmation and edit flows replace the composer bar — the house pattern
 * (`MainContent`'s edit/delete bars); no modals.
 */
export const VaultPage: React.FC = observer(() => {
  const { t } = useTranslation("vault");
  const [view, setView] = useState<VaultView>("chat");
  const [searchQuery, setSearchQuery] = useState("");
  const [editingEntry, setEditingEntry] = useState<VaultMessage | null>(null);
  const [editDraft, setEditDraft] = useState("");
  const [pendingDeleteId, setPendingDeleteId] = useState<string | null>(null);

  const { data: entries = [], isLoading } = useVaultMessages();
  const sendMutation = useSendVaultMessage();
  const editMutation = useEditVaultMessage();
  const deleteMutation = useDeleteVaultMessage();
  const pinMutation = useSetVaultMessagePinned();

  const chatInputRef = useRef<ChatInputHandle>(null);
  const logRef = useRef<HTMLDivElement>(null);

  // Keep the log pinned to the newest entry, chat-style.
  useEffect(() => {
    if (view === "chat" && logRef.current) {
      logRef.current.scrollTop = logRef.current.scrollHeight;
    }
  }, [entries.length, view]);

  // Client-side search over decrypted entries — the server cannot search
  // what it cannot read.
  const visibleEntries = useMemo(() => {
    const needle = searchQuery.trim().toLowerCase();
    if (!needle) {
      return entries;
    }
    return entries.filter((entry) => {
      const parsed = parseContent(entry.content);
      const haystacks = [
        parsed.text,
        ...(parsed.attachments ?? []).map((a) => a.filename),
      ];
      return haystacks.some((h) => h.toLowerCase().includes(needle));
    });
  }, [entries, searchQuery]);

  // Every attachment across the vault, newest entry first — the media roll.
  const allAttachments = useMemo(() => {
    const out: MessageAttachment[] = [];
    const seen = new Set<string>();
    for (let i = entries.length - 1; i >= 0; i--) {
      for (const attachment of parseContent(entries[i].content).attachments ?? []) {
        if (!seen.has(attachment.id)) {
          seen.add(attachment.id);
          out.push(attachment);
        }
      }
    }
    return out;
  }, [entries]);

  const handleSend = useCallback(
    async (text: string, attachments: Attachment[]) => {
      const contentText = text.trim();
      if (!contentText && attachments.length === 0) {
        return;
      }
      const content = await buildMessageContent(attachments, contentText);
      sendMutation.mutate({ content });
    },
    [sendMutation],
  );

  const startEdit = useCallback((entry: VaultMessage) => {
    setPendingDeleteId(null);
    setEditDraft(parseContent(entry.content).text);
    setEditingEntry(entry);
  }, []);

  const confirmEdit = useCallback(() => {
    if (!editingEntry) {
      return;
    }
    const parsed = parseContent(editingEntry.content);
    const attachments = parsed.attachments ?? [];
    // An entry with files keeps them across a text edit: rebuild the same
    // envelope with the new caption. The wire field names mirror
    // `buildMessageContent`.
    const newContent =
      attachments.length > 0
        ? JSON.stringify({
            _att: attachments.map((a) => ({
              key: a.object_key,
              url: "",
              name: a.filename,
              ct: a.content_type,
              size: a.file_size,
              hash: a.content_hash,
              bh: a.blurhash,
              w: a.width,
              h: a.height,
            })),
            ...(editDraft.trim() ? { _txt: editDraft.trim() } : {}),
          })
        : editDraft.trim();
    if (!newContent) {
      return;
    }
    editMutation.mutate(
      { id: editingEntry.id, newContent },
      { onSuccess: () => setEditingEntry(null) },
    );
  }, [editingEntry, editDraft, editMutation]);

  const confirmDelete = useCallback(() => {
    if (!pendingDeleteId) {
      return;
    }
    deleteMutation.mutate(
      { id: pendingDeleteId },
      { onSuccess: () => setPendingDeleteId(null) },
    );
  }, [pendingDeleteId, deleteMutation]);

  // Escape backs out of the edit/delete bars before the window-level
  // nav.back shortcut can navigate the page away — the MainContent pattern.
  useEffect(() => {
    if (!editingEntry && !pendingDeleteId) {
      return;
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopImmediatePropagation();
        setEditingEntry(null);
        setPendingDeleteId(null);
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [editingEntry, pendingDeleteId]);

  const viewToggle = (
    <div className="flex items-center gap-1" role="tablist" aria-label={t("viewToggle")}>
      <button
        type="button"
        role="tab"
        aria-selected={view === "chat"}
        data-testid="vault-view-chat"
        onClick={() => setView("chat")}
        className={`flex items-center gap-1 rounded border border-line px-2 py-0.5 text-2xs uppercase tracking-widest ${
          view === "chat" ? "bg-accent text-bg" : "text-muted hover:text-fg"
        }`}
      >
        <MessageSquare size={12} aria-hidden />
        {t("viewChat")}
      </button>
      <button
        type="button"
        role="tab"
        aria-selected={view === "media"}
        data-testid="vault-view-media"
        onClick={() => setView("media")}
        className={`flex items-center gap-1 rounded border border-line px-2 py-0.5 text-2xs uppercase tracking-widest ${
          view === "media" ? "bg-accent text-bg" : "text-muted hover:text-fg"
        }`}
      >
        <ImageIcon size={12} aria-hidden />
        {t("viewMedia")}
      </button>
    </div>
  );

  return (
    <div className="flex h-full flex-col">
      <div className="flex h-bar flex-shrink-0 items-center gap-3 border-b border-line px-4 font-mono text-xs text-muted">
        <VaultIcon size={14} aria-hidden />
        <span className="flex-1">{t("title")}</span>
        {viewToggle}
      </div>

      {view === "media" ? (
        <div className="min-h-0 flex-1">
          <MediaGalleryView
            attachments={allAttachments}
            emptyLabel={t("media.empty")}
          />
        </div>
      ) : (
        <>
          <div className="flex flex-shrink-0 items-center gap-2 border-b border-line px-4 py-1.5">
            <Search size={14} aria-hidden className="text-muted" />
            <input
              data-testid="vault-search"
              type="text"
              value={searchQuery}
              onChange={(event) => setSearchQuery(event.target.value)}
              placeholder={t("searchPlaceholder")}
              aria-label={t("searchPlaceholder")}
              autoComplete="off"
              autoCorrect="off"
              autoCapitalize="off"
              spellCheck={false}
              className="flex-1 border-0 bg-transparent font-mono text-sm text-fg outline-none placeholder:text-muted"
            />
            {searchQuery && (
              <button
                type="button"
                onClick={() => setSearchQuery("")}
                aria-label={t("clearSearch")}
                className="icon-btn-sm flex-shrink-0"
              >
                <X size={16} aria-hidden />
              </button>
            )}
          </div>

          <div ref={logRef} className="min-h-0 flex-1 overflow-y-auto px-4 py-2">
            {isLoading ? null : visibleEntries.length === 0 ? (
              <p className="py-8 text-center font-mono text-sm text-muted" data-testid="vault-empty">
                {searchQuery ? t("searchEmpty") : t("empty")}
              </p>
            ) : (
              <ul className="flex flex-col">
                {visibleEntries.map((entry, index) => (
                  <VaultEntryRow
                    key={entry.id}
                    entry={entry}
                    showDayDivider={
                      index === 0 ||
                      new Date(entry.created_at).toDateString() !==
                        new Date(visibleEntries[index - 1].created_at).toDateString()
                    }
                    onEdit={startEdit}
                    onDelete={(id) => {
                      setEditingEntry(null);
                      setPendingDeleteId(id);
                    }}
                    onTogglePin={(e) =>
                      pinMutation.mutate({ id: e.id, pinned: !e.pinned })
                    }
                  />
                ))}
              </ul>
            )}
          </div>

          {editingEntry ? (
            <EditMessageBar
              key={editingEntry.id}
              testId="vault-edit-bar"
              inputTestId="vault-edit-input"
              cancelTestId="vault-edit-cancel"
              heading={t("edit.heading")}
              cancelLabel={t("edit.cancel")}
              hint={t("edit.hint")}
              value={editDraft}
              onChange={setEditDraft}
              onSave={confirmEdit}
              onCancel={() => setEditingEntry(null)}
              isSaving={editMutation.isPending}
            />
          ) : pendingDeleteId ? (
            <div data-testid="vault-delete-bar">
              <div className="flex flex-shrink-0 items-center gap-2 border-t border-line bg-surface px-4 py-1.5">
                <span className="flex-1 font-mono text-2xs uppercase tracking-widest text-muted">
                  {t("delete.heading")}
                </span>
                <button
                  data-testid="vault-delete-cancel"
                  onClick={() => setPendingDeleteId(null)}
                  aria-label={t("delete.cancel")}
                  className="icon-btn-sm flex-shrink-0"
                >
                  <X size={20} aria-hidden="true" />
                </button>
              </div>
              <div className="flex items-center justify-between gap-4 bg-surface px-4 pb-3 pt-2">
                <p className="font-mono text-xs text-dim">{t("delete.body")}</p>
                <Button
                  data-testid="vault-delete-confirm"
                  variant="danger"
                  onClick={confirmDelete}
                  isLoading={deleteMutation.isPending}
                  loadingText={t("delete.deleting")}
                  autoFocus
                >
                  {t("delete.confirm")}
                </Button>
              </div>
            </div>
          ) : (
            <div className="flex-shrink-0" data-testid="vault-composer">
              <ChatInput
                ref={chatInputRef}
                onSend={handleSend}
                placeholder={t("placeholder")}
                draftKey="vault"
              />
            </div>
          )}
        </>
      )}
    </div>
  );
});

interface VaultEntryRowProps {
  entry: VaultMessage;
  showDayDivider: boolean;
  onEdit: (entry: VaultMessage) => void;
  onDelete: (id: string) => void;
  onTogglePin: (entry: VaultMessage) => void;
}

/** One vault entry: content + attachments, hover actions at the row end. */
const VaultEntryRow: React.FC<VaultEntryRowProps> = ({
  entry,
  showDayDivider,
  onEdit,
  onDelete,
  onTogglePin,
}) => {
  const { t } = useTranslation("vault");
  const parsed = parseContent(entry.content);
  const createdMs = new Date(entry.created_at).getTime();

  return (
    <li data-testid="vault-entry">
      {showDayDivider && (
        <div className="flex items-center gap-3 py-2">
          <div className="h-px flex-1 bg-line" />
          <span className="font-mono text-2xs uppercase tracking-widest text-muted">
            {formatDayDivider(createdMs)}
          </span>
          <div className="h-px flex-1 bg-line" />
        </div>
      )}
      <div className="group flex gap-3 rounded px-1 py-1 hover:bg-hover">
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-2">
            <span className="font-mono text-2xs text-muted">
              {formatClockTime(createdMs)}
            </span>
            {entry.pinned && (
              <span
                className="flex items-center gap-1 font-mono text-2xs uppercase tracking-widest text-accent"
                data-testid="vault-entry-pinned"
              >
                <Pin size={10} aria-hidden />
                {t("entry.pinned")}
              </span>
            )}
            {entry.updated_at !== entry.created_at && (
              <span className="font-mono text-2xs text-muted">{t("entry.edited")}</span>
            )}
          </div>
          {parsed.text && (
            <p className="whitespace-pre-wrap break-words text-sm text-fg">
              <LinkifiedText text={parsed.text} />
            </p>
          )}
          {parsed.attachments && parsed.attachments.length > 0 && (
            <div className="mt-1 flex flex-wrap gap-1">
              {parsed.attachments.map((attachment) => (
                <AttachmentDisplay key={attachment.id} attachment={attachment} />
              ))}
            </div>
          )}
        </div>
        <div className="flex h-6 flex-shrink-0 items-center gap-2 opacity-0 transition-opacity focus-within:opacity-100 group-hover:opacity-100">
          <button
            type="button"
            data-testid="vault-entry-pin"
            onClick={() => onTogglePin(entry)}
            aria-label={entry.pinned ? t("entry.unpin") : t("entry.pin")}
            title={entry.pinned ? t("entry.unpin") : t("entry.pin")}
            className={`p-0.5 outline-none hover:text-accent focus:bg-accent focus:text-bg ${
              entry.pinned ? "text-accent" : "text-muted"
            }`}
          >
            <Pin size={16} aria-hidden fill={entry.pinned ? "currentColor" : "none"} />
          </button>
          <button
            type="button"
            data-testid="vault-entry-edit"
            onClick={() => onEdit(entry)}
            aria-label={t("entry.edit")}
            title={t("entry.edit")}
            className="p-0.5 text-muted outline-none hover:text-accent focus:bg-accent focus:text-bg"
          >
            <Pencil size={16} aria-hidden />
          </button>
          <button
            type="button"
            data-testid="vault-entry-delete"
            onClick={() => onDelete(entry.id)}
            aria-label={t("entry.delete")}
            title={t("entry.delete")}
            className="p-0.5 text-muted outline-none hover:text-danger focus:bg-danger focus:text-bg"
          >
            <Trash2 size={16} aria-hidden />
          </button>
        </div>
      </div>
    </li>
  );
};
