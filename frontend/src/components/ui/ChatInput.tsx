import React, { useState, useRef, useCallback, useEffect, useImperativeHandle } from "react";
import { useTranslation } from "react-i18next";
import {
  dialogOpen,
  readFile,
  stat,
  readClipboardFiles,
  readClipboardImage,
  stageAttachment,
  discardStagedAttachment,
} from "../../bridge";
import { ChevronRight, Plus, X, Film, Music } from "lucide-react";
import { EmojiPickerButton } from "../Emoji/EmojiPickerButton";
import { getFileIcon, mimeFromName } from "../../utils/fileIcon";
import { captureVideoPoster } from "../../utils/imageProcessing";
import { formatFileSize } from "../../utils/format";
import { observer } from "mobx-react-lite";
import { dropTargetStore } from "../../stores/dropTargetStore";
import { getDraft, setDraft } from "../../utils/drafts";
import {
  mentionsAll,
  mentionQueryAt,
  applyMention,
  rankMentionCandidates,
  type MentionCandidate,
} from "../../utils/mentions";
import { useMentionCandidates } from "../../hooks/queries/useMentionCandidates";
import { useSkin } from "../../hooks/queries/usePreferences";
import {
  applyShortcode,
  completedShortcodeAt,
  rankShortcodeEntries,
  resolveShortcode,
  shortcodeQueryAt,
  type ShortcodeEntry,
} from "../Emoji/emojiShortcodeQuery";
import { useShortcodeEntries } from "../Emoji/useShortcodeEntries";
import { RichTextInput, type RichTextInputHandle } from "./RichTextInput";
import { MentionSuggestList } from "./MentionSuggestList";
import { EmojiSuggestList } from "./EmojiSuggestList";
import { probeRender } from "../../utils/renderProbe";

/**
 * Where a queued attachment's bytes are. `path`: a file the USER already has
 * (picker, OS drop), never deleted by us. `staged`: bytes with no path (paste,
 * `File` drop) held in backend memory (`pollis_core::commands::staging`), so
 * there is no temp file to forget to delete.
 */
export type AttachmentSource =
  | { kind: "path"; path: string }
  | { kind: "staged"; id: string };

export interface Attachment {
  id: string;
  source: AttachmentSource | null;
  name: string;
  size: number;      // bytes (0 if unknown)
  mimeType: string;
  preview?: string;  // blob URL for image/video poster previews
  type: "image" | "video" | "audio" | "file";
  loading?: boolean; // true while source/preview is still being prepared
}

export function stagedIdOf(attachment: Attachment): string | null {
  return attachment.source?.kind === "staged" ? attachment.source.id : null;
}

export interface ChatInputHandle {
  addFiles: (files: File[]) => void;
  focus: () => void;
  /** Put a failed send's draft back (#1497): text only into an empty input,
   *  attachments only into an empty tray, never overwriting newer work. */
  restore: (text: string, attachments: Attachment[]) => void;
  send: () => void;
}

interface ChatInputProps {
  onSend: (message: string, attachments: Attachment[]) => void;
  placeholder?: string;
  disabled?: boolean;
  autoFocus?: boolean;
  className?: string;
  maxAttachments?: number;
  // Every keystroke (and after send/clear); drives the typing publisher.
  onValueChange?: (value: string) => void;
  // Key into the in-memory draft store (`utils/drafts.ts`): seeds the input,
  // written per keystroke, cleared on send. Pass `null` (not omit) while the
  // room id loads; a change re-syncs within the same mount.
  draftKey?: string | null;
  // A standalone `@all` will notify every member (group channels only); gates
  // the live hint.
  canNotifyAll?: boolean;
  // ArrowUp on an empty input or the first line. Return true to claim the key
  // (focus moves into the log), false to keep native caret movement.
  onHistoryUp?: () => boolean;
}

function typeFromMime(mime: string): Attachment["type"] {
  if (mime.startsWith("image/")) { return "image"; }
  if (mime.startsWith("video/")) { return "video"; }
  if (mime.startsWith("audio/")) { return "audio"; }
  return "file";
}

const PREVIEW_SIZE = 80;

const AttachmentPreview: React.FC<{
  attachment: Attachment;
  onRemove: (id: string) => void;
  onExpand: (url: string, type: "image" | "video") => void;
}> = ({ attachment, onRemove, onExpand }) => {
  const { t } = useTranslation("common");
  const hasVisualPreview = attachment.type === "image" || attachment.type === "video";
  const canExpand = hasVisualPreview && !!attachment.preview && !attachment.loading;

  return (
    <div className="relative flex-shrink-0" style={{ width: PREVIEW_SIZE }}>
      <div
        className="flex items-center justify-center overflow-hidden border-2 border-line bg-surface-high"
        style={{
          width: PREVIEW_SIZE,
          height: PREVIEW_SIZE,
          borderRadius: 8,
          cursor: canExpand ? "zoom-in" : "default",
        }}
        onClick={() => {
          if (canExpand) {
            onExpand(attachment.preview!, attachment.type as "image" | "video");
          }
        }}
      >
        {attachment.loading ? (
          <span className="text-sm font-mono text-muted" style={{ animation: "pulse 1.5s ease-in-out infinite" }}>…</span>
        ) : attachment.preview ? (
          <img src={attachment.preview} alt={attachment.name} className="w-full h-full object-cover" style={{ borderRadius: 6 }} />
        ) : attachment.type === "video" ? (
          <Film size={28} className="text-dim" />
        ) : attachment.type === "audio" ? (
          <Music size={28} className="text-dim" />
        ) : (() => {
          const Icon = getFileIcon(attachment.name);
          return <Icon size={28} className="text-dim" />;
        })()}
      </div>
      <div
        className="mt-0.5 text-xs font-mono truncate text-muted"
        style={{ maxWidth: PREVIEW_SIZE }}
        title={attachment.name}
      >
        {attachment.name}
      </div>
      <button
        onClick={() => onRemove(attachment.id)}
        aria-label={t("composer.removeAttachment", { name: attachment.name })}
        className="absolute flex items-center justify-center bg-surface-high border border-line-strong text-dim"
        style={{
          top: -6,
          right: -6,
          width: 22,
          height: 22,
          borderRadius: 4,
        }}
      >
        <X className="w-3.5 h-3.5" />
      </button>
    </div>
  );
};

const ChatInputInner: React.ForwardRefRenderFunction<ChatInputHandle, ChatInputProps> = ({
  onSend,
  placeholder,
  disabled = false,
  autoFocus = false,
  className = "",
  maxAttachments = 10,
  onValueChange,
  draftKey = null,
  canNotifyAll = false,
  onHistoryUp,
}, ref) => {
  probeRender("ChatInput");
  const { t } = useTranslation("common");
  // Resolved at render so the fallback follows a language change.
  const resolvedPlaceholder = placeholder ?? t("composer.placeholder");
  const [message, setMessage] = useState(() => getDraft(draftKey));
  // Re-sync on draftKey change during render ("previous prop in state"), so the
  // input never flashes a stale value as a useEffect would.
  const [prevDraftKey, setPrevDraftKey] = useState(draftKey);
  if (draftKey !== prevDraftKey) {
    setPrevDraftKey(draftKey);
    setMessage(getDraft(draftKey));
  }
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [isFocused, setIsFocused] = useState(false);
  const [expandedPreview, setExpandedPreview] = useState<{ url: string; type: "image" | "video" } | null>(null);
  const inputRef = useRef<RichTextInputHandle>(null);

  // Insert at the caret, not the end. The input does the splice: a
  // `contentEditable` loses its selection across the picker's blur.
  const insertAtCursor = useCallback((text: string) => {
    inputRef.current?.insertText(text);
  }, []);

  useEffect(() => {
    if (!expandedPreview) { return; }
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopImmediatePropagation();
        setExpandedPreview(null);
      }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true });
  }, [expandedPreview]);

  // Refocus after the pre-send preview lightbox closes.
  const prevExpandedPreviewRef = useRef(expandedPreview);
  useEffect(() => {
    if (prevExpandedPreviewRef.current && !expandedPreview) {
      inputRef.current?.focus();
    }
    prevExpandedPreviewRef.current = expandedPreview;
  }, [expandedPreview]);

  // Refocus whenever the attachment count changes (drops while the picker is
  // open, removals, pastes); a prev-value ref keeps the initial mount from
  // stealing focus.
  const prevAttachmentLengthRef = useRef(attachments.length);
  useEffect(() => {
    if (prevAttachmentLengthRef.current !== attachments.length) {
      inputRef.current?.focus();
    }
    prevAttachmentLengthRef.current = attachments.length;
  }, [attachments.length]);

  // Path-based attachments: picker + OS drag-drop.
  const handlePaths = useCallback(async (paths: string[]) => {
    const existingPaths = new Set(
      attachments
        .map((a) => (a.source?.kind === "path" ? a.source.path : null))
        .filter((p): p is string => p !== null),
    );
    const newPaths = paths.filter((p) => !existingPaths.has(p));
    const remaining = maxAttachments - attachments.length;
    const candidates = newPaths.slice(0, remaining);
    if (candidates.length === 0) { return; }

    const checks = await Promise.all(
      candidates.map(async (p) => {
        try {
          const info = await stat(p);
          return info.isDirectory ? null : p;
        } catch {
          // stat failed: let it through and fail gracefully later
          return p;
        }
      })
    );
    const toProcess = checks.filter((p): p is string => p !== null);
    if (toProcess.length === 0) { return; }

    // Stubs first so cards show immediately; typed with the narrowed source.
    type PathStub = Attachment & { source: { kind: "path"; path: string } };
    const stubs: PathStub[] = toProcess.map((p) => {
      const name = p.split(/[\\/]/).pop() ?? p;
      const mime = mimeFromName(name);
      const type = typeFromMime(mime);
      return {
        id: `${Date.now()}-${Math.random()}`,
        source: { kind: "path", path: p },
        name,
        size: 0,
        mimeType: mime,
        type,
        loading: type === "image" || type === "video",
      };
    });
    setAttachments((prev) => [...prev, ...stubs]);

    await Promise.all([
      ...stubs
        .filter((s) => s.type === "image")
        .map(async (stub) => {
          let preview: string | undefined;
          try {
            const bytes = await readFile(stub.source.path);
            preview = URL.createObjectURL(new Blob([bytes], { type: stub.mimeType }));
          } catch {
            // no preview, fall back to file icon
          }
          setAttachments((prev) =>
            prev.map((a) => a.id === stub.id ? { ...a, preview, loading: false } : a)
          );
        }),
      // Videos: readFile → blob URL → poster. Not convertFileSrc, which
      // percent-encodes Linux paths into URLs WebKit can't serve.
      ...stubs
        .filter((s) => s.type === "video")
        .map(async (stub) => {
          let preview: string | undefined;
          try {
            const bytes = await readFile(stub.source.path);
            const blobSrc = URL.createObjectURL(new Blob([bytes], { type: stub.mimeType }));
            preview = (await captureVideoPoster(blobSrc))?.url;
            URL.revokeObjectURL(blobSrc);
          } catch {
            // no preview — Film icon will show
          }
          setAttachments((prev) =>
            prev.map((a) => a.id === stub.id ? { ...a, preview, loading: false } : a)
          );
        }),
    ]);
  }, [attachments, maxAttachments]);

  const handlePickFiles = useCallback(async () => {
    if (attachments.length >= maxAttachments) { return; }
    try {
      const result = await dialogOpen({
        multiple: true,
        directory: false,
        title: t("composer.filePickerTitle"),
      }).catch((err) => { console.error("[ChatInput] open dialog failed:", err); return null; });
      if (!result) { return; }
      await handlePaths(Array.isArray(result) ? result : [result]);
    } finally {
      // The native dialog steals focus and the webview doesn't restore it.
      inputRef.current?.focus();
    }
  }, [attachments.length, maxAttachments, handlePaths, t]);

  // Paste: File objects, staged in backend memory.
  const handleBrowserFile = useCallback(async (file: File) => {
    if (attachments.length >= maxAttachments) { return; }
    // De-dupe by name+size; pasted files have no path.
    if (attachments.some((a) => a.name === file.name && a.size === file.size)) { return; }

    const id = `${Date.now()}-${Math.random()}`;
    const mime = file.type || mimeFromName(file.name);
    const type = typeFromMime(mime);
    const isImg = type === "image";
    const isVid = type === "video";

    const preview = isImg ? URL.createObjectURL(file) : undefined;

    setAttachments((prev) => [
      ...prev,
      {
        id,
        source: null,
        name: file.name,
        size: file.size,
        mimeType: mime,
        preview,
        type,
        loading: true,
      },
    ]);

    let videoPoster: string | undefined;
    if (isVid) {
      const blobSrc = URL.createObjectURL(file);
      videoPoster = (await captureVideoPoster(blobSrc).catch(() => null))?.url;
      URL.revokeObjectURL(blobSrc);
    }

    // Bytes go to backend memory; the only handle is an opaque id.
    const staged = await stageAttachment(
      new Uint8Array(await file.arrayBuffer()),
    ).catch((err) => {
      console.error("[ChatInput] stageAttachment failed:", err);
      return null;
    });

    if (!staged) {
      if (preview) { URL.revokeObjectURL(preview); }
      if (videoPoster) { URL.revokeObjectURL(videoPoster); }
      setAttachments((prev) => prev.filter((a) => a.id !== id));
      return;
    }

    setAttachments((prev) => {
      // Card removed while in flight: release the bytes.
      if (!prev.some((a) => a.id === id)) {
        void discardStagedAttachment(staged.id);
        return prev;
      }
      return prev.map((a) => a.id === id
        ? {
            ...a,
            source: { kind: "staged", id: staged.id } as const,
            preview: preview ?? videoPoster,
            loading: false,
          }
        : a);
    });
  }, [attachments, maxAttachments]);

  // Rebuilt each render: `handleSend` reads the latest state.
  const sendRef = useRef<() => void>(() => {});
  useImperativeHandle(ref, () => ({
    addFiles: (files: File[]) => { files.forEach(handleBrowserFile); },
    focus: () => { inputRef.current?.focus(); },
    restore: (text: string, restored: Attachment[]) => {
      setMessage((current) => {
        if (current.trim()) {
          return current;
        }
        setDraft(draftKey, text);
        return text;
      });
      setAttachments((current) => (current.length > 0 ? current : restored));
    },
    send: () => { sendRef.current(); },
  }), [handleBrowserFile, draftKey]);

  // Registered as the drop target so AppShell shows the overlay only where a
  // file can land.
  useEffect(() => {
    const { register, unregister } = dropTargetStore;
    register();
    return unregister;
  }, []);

  useEffect(() => {
    const handler = (e: Event) => {
      const paths: string[] = (e as CustomEvent<{ paths: string[] }>).detail.paths;
      handlePaths(paths);
    };
    window.addEventListener("pollis:pathdrop", handler);
    return () => window.removeEventListener("pollis:pathdrop", handler);
  }, [handlePaths]);

  const hasLoadingAttachments = attachments.some((a) => a.loading);

  // Mirrors the backend's `mentions_all()`.
  const willNotifyEveryone = canNotifyAll && mentionsAll(message);

  // @mention autocomplete (#843), from the conversation roster only (see
  // useMentionCandidates).
  const skin = useSkin();
  const mentionCandidates = useMentionCandidates();
  // From the async `selectionchange`: fine for opening suggestions, NOT what the
  // ArrowUp hand-off reads.
  const [caret, setCaret] = useState(0);
  const [activeIndex, setActiveIndex] = useState(0);
  const [mentionDismissed, setMentionDismissed] = useState(false);
  // Applied in the effect below once React has committed the new value.
  const pendingCaretRef = useRef<number | null>(null);

  const mentionQuery = mentionDismissed ? null : mentionQueryAt(message, caret);
  const suggestions = mentionQuery
    ? rankMentionCandidates(mentionCandidates, mentionQuery.query)
    : [];

  // Terminal ghost: append-only, so a prefix match, offered only with the caret
  // at end of line.
  const ghostTarget =
    skin === "terminal" && mentionQuery && caret === message.length
      ? suggestions.find((c) =>
          c.username.toLowerCase().startsWith(mentionQuery.query),
        )
      : undefined;
  const ghostText = ghostTarget
    ? ghostTarget.username.slice(mentionQuery!.query.length)
    : "";

  const showSuggestList = skin === "refined" && suggestions.length > 0;

  // `:shortcode:` autocomplete. Mentions take precedence for Enter / Tab / Esc;
  // in practice no caret sits inside both queries, since neither alphabet
  // contains the other's trigger.
  const [emojiDismissed, setEmojiDismissed] = useState(false);
  const emojiQuery =
    emojiDismissed || mentionQuery ? null : shortcodeQueryAt(message, caret);
  // Loaded on focus, not the first colon, so `:tada:` typed straight out
  // resolves to the group's own emoji first time; still after first paint (#874).
  const shortcodeEntries = useShortcodeEntries(isFocused || emojiQuery !== null);
  const emojiSuggestions = emojiQuery
    ? rankShortcodeEntries(shortcodeEntries, emojiQuery.query)
    : [];

  // Terminal ghost: prefix match at end of line, closing ':' included.
  const emojiGhostTarget =
    skin === "terminal" && emojiQuery && caret === message.length
      ? emojiSuggestions.find((e) => e.shortcode.startsWith(emojiQuery.query))
      : undefined;
  const emojiGhostText = emojiGhostTarget
    ? `${emojiGhostTarget.shortcode.slice(emojiQuery!.query.length)}:`
    : "";

  const showEmojiList = skin === "refined" && emojiSuggestions.length > 0;

  const activeListLength = showSuggestList ? suggestions.length : emojiSuggestions.length;

  useEffect(() => {
    setActiveIndex((prev) => (prev < activeListLength ? prev : 0));
  }, [activeListLength]);

  useEffect(() => {
    const pending = pendingCaretRef.current;
    if (pending === null || !inputRef.current) {
      return;
    }
    pendingCaretRef.current = null;
      // Passive, so it runs after RichTextInput rebuilt its projection.
    inputRef.current.setSelection(pending);
    setCaret(pending);
  }, [message]);

  const acceptMention = useCallback((candidate: MentionCandidate) => {
    const next = applyMention(message, caret, candidate.username);
    if (next.text === message) {
      return;
    }
    pendingCaretRef.current = next.caret;
    setMessage(next.text);
    setDraft(draftKey, next.text);
    onValueChange?.(next.text);
    setActiveIndex(0);
  }, [message, caret, draftKey, onValueChange]);

  // List or ghost acceptance adds a trailing space; typing the closing ':'
  // yourself doesn't (mid-word).
  const acceptEmoji = useCallback((entry: ShortcodeEntry) => {
    const query = shortcodeQueryAt(message, caret);
    if (!query) {
      return;
    }
    const next = applyShortcode(message, query.start, query.end, entry.insertText, true);
    pendingCaretRef.current = next.caret;
    setMessage(next.text);
    setDraft(draftKey, next.text);
    onValueChange?.(next.text);
    setActiveIndex(0);
  }, [message, caret, draftKey, onValueChange]);

  const handleSend = () => {
    if (!message.trim() && attachments.length === 0) { return; }
    if (hasLoadingAttachments) { return; }
    onSend(message.trim(), attachments);
    setMessage("");
    setDraft(draftKey, "");
    // Tell the typing publisher we stopped, rather than waiting out the TTL.
    onValueChange?.("");
    // Don't revoke preview blob URLs: optimistic stubs may still reference them.
    setAttachments([]);
    inputRef.current?.focus();
  };
  sendRef.current = handleSend;

  const handleKeyDown = (e: React.KeyboardEvent) => {
    // While a suggestion is offered, Enter accepts it rather than sending.
    if (showSuggestList) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setActiveIndex((i) => (i + 1) % suggestions.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setActiveIndex((i) => (i - 1 + suggestions.length) % suggestions.length);
        return;
      }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        acceptMention(suggestions[activeIndex]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        setMentionDismissed(true);
        return;
      }
    }

    // Terminal: Tab accepts the ghosted completion.
    if (ghostTarget) {
      if (e.key === "Tab") {
        e.preventDefault();
        acceptMention(ghostTarget);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        setMentionDismissed(true);
        return;
      }
    }

    // Same key contract, reached only with no mention query open.
    if (showEmojiList) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setActiveIndex((i) => (i + 1) % emojiSuggestions.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setActiveIndex((i) => (i - 1 + emojiSuggestions.length) % emojiSuggestions.length);
        return;
      }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        acceptEmoji(emojiSuggestions[activeIndex]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        setEmojiDismissed(true);
        return;
      }
    }

    if (emojiGhostTarget) {
      if (e.key === "Tab") {
        e.preventDefault();
        acceptEmoji(emojiGhostTarget);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        setEmojiDismissed(true);
        return;
      }
    }

    // Bash-history hand-off. "First line" is the first LOGICAL line. The
    // selection is read synchronously, not from `caret`: async
    // `selectionchange` would judge a quick second ArrowUp on a stale offset.
    if (e.key === "ArrowUp" && onHistoryUp) {
      const selection = inputRef.current?.selection() ?? { start: 0, end: 0 };
      const collapsed = selection.start === selection.end;
      const onFirstLine = !message.slice(0, selection.start).includes("\n");
      if (collapsed && (message === "" || onFirstLine)) {
        if (onHistoryUp()) {
          e.preventDefault();
          inputRef.current?.blur();
          return;
        }
      }
    }

    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  // True when the paste was a file and the text path must not run.
  const handlePaste = useCallback((e: React.ClipboardEvent): boolean => {
    // Images from web content arrive as DataTransferItem files.
    const items = e.clipboardData.items;
    let hasFiles = false;
    for (let i = 0; i < items.length; i++) {
      if (items[i].kind === "file") {
        const file = items[i].getAsFile();
        if (file) { handleBrowserFile(file); hasFiles = true; }
      }
    }
    if (hasFiles) {
      return true;
    }

    // WebKit hides OS file-manager copies; read them via the Rust clipboard
    // command. Async and non-claiming, so text still inserts alongside.
    readClipboardFiles().then((paths) => {
      if (paths.length > 0) {
        handlePaths(paths);
        return;
      }
      // WebKitGTK hides clipboard images too: fetch PNG bytes from the OS
      // clipboard and stage them like a pasted `File`.
      readClipboardImage().then((bytes) => {
        if (bytes) {
          handleBrowserFile(
            new File([bytes], "pasted-image.png", { type: "image/png" }),
          );
        }
      }).catch(() => { /* no image on clipboard */ });
    }).catch(() => { /* clipboard unreadable */ });
    return false;
  }, [handleBrowserFile, handlePaths]);

  const removeAttachment = useCallback((id: string) => {
    setAttachments((prev) => {
      const att = prev.find((a) => a.id === id);
      if (att?.preview) { URL.revokeObjectURL(att.preview); }
      // Release the staged bytes, not just the card.
      const stagedId = att ? stagedIdOf(att) : null;
      if (stagedId) { void discardStagedAttachment(stagedId); }
      return prev.filter((a) => a.id !== id);
    });
  }, []);

  // On unmount, discard whatever is still staged. Reads a ref so the effect has
  // no deps; depending on `attachments` would discard bytes still in use.
  const attachmentsRef = useRef(attachments);
  attachmentsRef.current = attachments;
  useEffect(() => {
    return () => {
      for (const att of attachmentsRef.current) {
        const stagedId = stagedIdOf(att);
        if (stagedId) { void discardStagedAttachment(stagedId); }
      }
    };
  }, []);

  return (
    <div
      className={`relative border-t border-line bg-bg ${className}`}
    >
      {/* Anchored with `absolute`, never a portal or fixed overlay. */}
      {showSuggestList && mentionQuery && (
        <MentionSuggestList
          candidates={suggestions}
          activeIndex={activeIndex}
          query={mentionQuery.query}
          onSelect={acceptMention}
          onHover={setActiveIndex}
        />
      )}

      {showEmojiList && emojiQuery && (
        <EmojiSuggestList
          entries={emojiSuggestions}
          activeIndex={activeIndex}
          query={emojiQuery.query}
          onSelect={acceptEmoji}
          onHover={setActiveIndex}
        />
      )}

      {attachments.length > 0 && (
        <div
          className="px-2 py-2 flex items-start gap-2 flex-wrap border-b border-line"
        >
          {attachments.map((att) => (
            <AttachmentPreview
              key={att.id}
              attachment={att}
              onRemove={removeAttachment}
              onExpand={(url, type) => setExpandedPreview({ url, type })}
            />
          ))}
        </div>
      )}

      {willNotifyEveryone && (
        <div
          className="px-3 py-1 flex items-center gap-1.5 text-xs font-mono text-accent border-b border-line"
        >
          <span style={{ fontWeight: 600 }}>@all</span>
          <span className="text-muted">
            {t("composer.allMentionHint")}
          </span>
        </div>
      )}

      {/* A single-line composer is exactly `--composer-h`; other bars floor on
          the same token so their borders align. */}
      <div className="flex items-start gap-1 px-2 py-1 min-h-composer">
        <button
          onClick={handlePickFiles}
          disabled={disabled || attachments.length >= maxAttachments}
          aria-label={t("composer.addAttachment")}
          className="pt-2 pb-1.5 px-1.5 flex-shrink-0 transition-colors text-muted enabled:hover:text-accent"
          style={{ opacity: disabled ? 0.4 : 1 }}
        >
          <Plus className="w-4 h-4" />
        </button>

        {/* `align="left"` so the panel grows rightwards: this trigger hugs the
            content region's left edge, which AppShell clips. h-8 centres the
            trigger on the first text line. */}
        <EmojiPickerButton
          onSelect={insertAtCursor}
          placement="up"
          align="left"
          className="h-8 flex items-center"
        />

        {/* At most one completion track offers a ghost, so one slot carries
            both, each keeping its own testid. */}
        <div className="relative flex-1 min-w-0">
          <RichTextInput
            ref={inputRef}
            testId="message-input"
            value={message}
            onChange={(next, nextCaret) => {
              setMentionDismissed(false);
              setEmojiDismissed(false);
              // Slack's direct substitution: typing the ':' that closes a known
              // shortcode replaces it. Single-character insertions only, so a
              // paste ending in ':' stays as pasted.
              if (next.length === message.length + 1 && next[nextCaret - 1] === ":") {
                const closed = completedShortcodeAt(next, nextCaret);
                const entry = closed
                  ? resolveShortcode(shortcodeEntries, closed.name)
                  : undefined;
                if (closed && entry) {
                  const applied = applyShortcode(
                    next,
                    closed.start,
                    closed.end,
                    entry.insertText,
                    false,
                  );
                  pendingCaretRef.current = applied.caret;
                  setMessage(applied.text);
                  setDraft(draftKey, applied.text);
                  onValueChange?.(applied.text);
                  return;
                }
              }
              setMessage(next);
              setDraft(draftKey, next);
              onValueChange?.(next);
              setCaret(nextCaret);
            }}
            onSelectionChange={(nextCaret) => {
              setCaret(nextCaret);
            }}
            onFocus={() => setIsFocused(true)}
            onBlur={() => setIsFocused(false)}
            onKeyDown={handleKeyDown}
            onPaste={handlePaste}
            placeholder={resolvedPlaceholder}
            disabled={disabled}
            autoFocus={autoFocus}
            ghost={ghostText || emojiGhostText}
            ghostTestId={ghostText ? "mention-ghost" : "emoji-ghost"}
            ghostClassName={isFocused ? "text-bg opacity-60" : "text-muted"}
            className={`chat-input-textarea w-full px-2 py-1 font-mono text-sm transition-colors border-0 whitespace-pre-wrap break-words overflow-y-auto${isFocused ? " is-focused" : ""}`}
            style={{
              lineHeight: "1.5rem",
              minHeight: "1.5rem",
              // Six lines, in rem so it tracks the font size.
              maxHeight: "9rem",
              borderRadius: "4px",
              background: isFocused ? "var(--c-accent)" : "var(--c-hover)",
              color: isFocused ? "var(--c-bg)" : "var(--c-text)",
              outline: "none",
              opacity: disabled ? 0.5 : 1,
            }}
            ariaLabel={t("composer.inputLabel")}
          />
        </div>

        <button
          onClick={handleSend}
          disabled={disabled || (!message.trim() && attachments.length === 0) || hasLoadingAttachments}
          data-testid="message-send-button"
          aria-label={t("composer.send")}
          className="pt-2 pb-1.5 px-1.5 flex-shrink-0 transition-colors text-muted enabled:hover:text-accent"
          style={{
            opacity: disabled || (!message.trim() && !attachments.length) ? 0.3 : 1,
          }}
        >
          <ChevronRight className="w-4 h-4 rtl-mirror" />
        </button>
      </div>

      {expandedPreview && (
        <div
          style={{
            position: "fixed",
            inset: 0,
            zIndex: 9999,
            background: "rgba(0,0,0,0.92)",
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            justifyContent: "center",
            cursor: "zoom-out",
          }}
          onClick={() => setExpandedPreview(null)}
        >
          {expandedPreview.type === "video" ? (
            <video
              autoFocus
              src={expandedPreview.url}
              controls
              style={{ maxWidth: "90vw", maxHeight: "85vh", cursor: "default", borderRadius: "1rem" }}
              onClick={(e) => e.stopPropagation()}
            />
          ) : (
            <img
              src={expandedPreview.url}
              alt={t("composer.previewAlt")}
              style={{ maxWidth: "90vw", maxHeight: "85vh", objectFit: "contain", cursor: "default", borderRadius: "1rem" }}
              onClick={(e) => e.stopPropagation()}
            />
          )}
          <button
            onClick={() => setExpandedPreview(null)}
            className="mt-3 text-xs font-mono transition-colors text-dim bg-transparent hover:bg-accent hover:text-black focus:outline-none focus:ring-2 focus:ring-accent focus:ring-offset-1 focus:ring-offset-black px-2 py-0.5"
            style={{ border: "1px solid transparent", borderRadius: 4, cursor: "pointer" }}
          >
            [esc]
          </button>
        </div>
      )}
    </div>
  );
};

export const ChatInput = observer(React.forwardRef(ChatInputInner));
