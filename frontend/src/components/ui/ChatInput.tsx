import React, { useState, useRef, useCallback, useEffect, useImperativeHandle } from "react";
import { useTranslation } from "react-i18next";
import {
  dialogOpen,
  writeFile,
  readFile,
  stat,
  tempDir,
  readClipboardFiles,
  readClipboardImageToTemp,
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
import { MentionGhost } from "./MentionGhost";
import { MentionSuggestList } from "./MentionSuggestList";

// Attachment carries a filesystem path so Rust can read the file directly —
// no bytes-over-IPC bottleneck, no size limit.
export interface Attachment {
  id: string;
  path: string;      // absolute filesystem path (empty while loading)
  name: string;
  size: number;      // bytes (0 if unknown)
  mimeType: string;
  preview?: string;  // blob URL for image/video poster previews
  type: "image" | "video" | "audio" | "file";
  loading?: boolean; // true while path/preview is still being prepared
}

export interface ChatInputHandle {
  addFiles: (files: File[]) => void;
  focus: () => void;
}

interface ChatInputProps {
  onSend: (message: string, attachments: Attachment[]) => void;
  placeholder?: string;
  disabled?: boolean;
  autoFocus?: boolean;
  className?: string;
  maxAttachments?: number;
  // Fires on every keystroke (and after send/clear). The parent uses this
  // to drive the typing indicator publisher; it stays optional so simpler
  // call sites that don't need it can ignore it entirely.
  onValueChange?: (value: string) => void;
  // Stable key for the in-memory draft store (`utils/drafts.ts`). When
  // present, the initial textarea value is seeded from the draft for this
  // key, every keystroke writes back, and a successful send clears the
  // entry. When undefined or null, the component behaves exactly as before
  // (no draft persistence between mounts). Pass `null` rather than omitting
  // the prop while the parent's room id is still loading — the component
  // re-syncs whenever this value changes within the same mount.
  draftKey?: string | null;
  // True when a standalone `@all` in the message will actually notify every
  // member — i.e. this is a group channel (DMs and other surfaces don't fan
  // out an `all_mention`). Gates the live "@all notifies everyone" composer
  // hint so it only appears where the mention does something.
  canNotifyAll?: boolean;
  // Bash-history hand-off: called when ArrowUp is pressed while the message
  // is empty or the caret sits on the first line. Return true to claim the
  // key — the input blurs and the parent takes focus into the message log;
  // return false (e.g. no messages to walk) to keep native caret movement.
  onHistoryUp?: () => boolean;
}

function typeFromMime(mime: string): Attachment["type"] {
  if (mime.startsWith("image/")) { return "image"; }
  if (mime.startsWith("video/")) { return "video"; }
  if (mime.startsWith("audio/")) { return "audio"; }
  return "file";
}

/// Write a browser File to the OS temp directory and return its path.
/// Used for paste and drag-and-drop, where no filesystem path is available.
async function writeToTemp(file: File): Promise<string> {
  const dir = await tempDir();
  const name = `pollis-${Date.now()}-${file.name}`;
  // Use forward slashes; on Windows Tauri normalises the separator.
  const path = `${dir}/${name}`;
  const bytes = new Uint8Array(await file.arrayBuffer());
  await writeFile(path, bytes);
  return path;
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
  const { t } = useTranslation("common");
  // Resolved at render, not as a default parameter, so the fallback follows
  // a language change.
  const resolvedPlaceholder = placeholder ?? t("composer.placeholder");
  const [message, setMessage] = useState(() => getDraft(draftKey));
  // Re-sync message when draftKey changes within the same mount (e.g. the
  // user navigates from #general to #random without unmounting MainContent).
  // Done during render via the "store previous prop in state" pattern so
  // the textarea never shows a stale value for a frame — a useEffect would
  // run after paint and produce a visible flash. Cheap: the compare is one
  // string === and the setState bails out when values match.
  const [prevDraftKey, setPrevDraftKey] = useState(draftKey);
  if (draftKey !== prevDraftKey) {
    setPrevDraftKey(draftKey);
    setMessage(getDraft(draftKey));
  }
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [isFocused, setIsFocused] = useState(false);
  // Lightbox for previewing attachments before send.
  const [expandedPreview, setExpandedPreview] = useState<{ url: string; type: "image" | "video" } | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Insert emoji text at the caret, not at the end — appending would be wrong
  // the moment someone picks an emoji mid-sentence, which is the common case.
  const insertAtCursor = useCallback((text: string) => {
    const el = textareaRef.current;
    setMessage((prev) => {
      const start = el?.selectionStart ?? prev.length;
      const end = el?.selectionEnd ?? prev.length;
      const next = prev.slice(0, start) + text + prev.slice(end);
      // Restore the caret after React has painted the new value.
      requestAnimationFrame(() => {
        const caret = start + text.length;
        el?.focus();
        el?.setSelectionRange(caret, caret);
      });
      return next;
    });
  }, []);

  // Close preview lightbox on Escape.
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

  // Refocus the textarea after the pre-send preview lightbox closes so
  // typing resumes immediately without an extra click.
  const prevExpandedPreviewRef = useRef(expandedPreview);
  useEffect(() => {
    if (prevExpandedPreviewRef.current && !expandedPreview) {
      textareaRef.current?.focus();
    }
    prevExpandedPreviewRef.current = expandedPreview;
  }, [expandedPreview]);

  // Refocus the textarea whenever the attachment count changes — covers
  // drag-drop into the app while the native picker is still open (the
  // picker's own pending `await open()` blocks `handlePickFiles`'
  // finally-block from running until the user dismisses the dialog),
  // remove-attachment, paste, and any other path that mutates the
  // attachment list. Compares against a prev-value ref so the initial
  // mount with `[]` doesn't steal focus from whatever the parent put it
  // on.
  const prevAttachmentLengthRef = useRef(attachments.length);
  useEffect(() => {
    if (prevAttachmentLengthRef.current !== attachments.length) {
      textareaRef.current?.focus();
    }
    prevAttachmentLengthRef.current = attachments.length;
  }, [attachments.length]);

  // ── Shared path-based attachment builder (picker + OS drag-drop) ─────────
  const handlePaths = useCallback(async (paths: string[]) => {
    // De-dupe against already-queued paths.
    const existingPaths = new Set(attachments.map((a) => a.path).filter(Boolean));
    const newPaths = paths.filter((p) => !existingPaths.has(p));
    const remaining = maxAttachments - attachments.length;
    const candidates = newPaths.slice(0, remaining);
    if (candidates.length === 0) { return; }

    // Filter out directories before adding stubs.
    const checks = await Promise.all(
      candidates.map(async (p) => {
        try {
          const info = await stat(p);
          return info.isDirectory ? null : p;
        } catch {
          // stat failed — let it through and fail gracefully later
          return p;
        }
      })
    );
    const toProcess = checks.filter((p): p is string => p !== null);
    if (toProcess.length === 0) { return; }

    // Add stubs immediately so the user sees cards right away.
    const stubs: Attachment[] = toProcess.map((p) => {
      const name = p.split(/[\\/]/).pop() ?? p;
      const mime = mimeFromName(name);
      const type = typeFromMime(mime);
      return {
        id: `${Date.now()}-${Math.random()}`,
        path: p,
        name,
        size: 0,
        mimeType: mime,
        type,
        // Images and videos need async work for previews.
        loading: type === "image" || type === "video",
      };
    });
    setAttachments((prev) => [...prev, ...stubs]);

    // Load previews for image and video stubs in parallel.
    await Promise.all([
      // Images: readFile → blob URL
      ...stubs
        .filter((s) => s.type === "image")
        .map(async (stub) => {
          let preview: string | undefined;
          try {
            const bytes = await readFile(stub.path);
            preview = URL.createObjectURL(new Blob([bytes], { type: stub.mimeType }));
          } catch {
            // no preview, fall back to file icon
          }
          setAttachments((prev) =>
            prev.map((a) => a.id === stub.id ? { ...a, preview, loading: false } : a)
          );
        }),
      // Videos: read file bytes → blob URL → poster frame capture.
      // We avoid convertFileSrc because it percent-encodes the path on Linux,
      // producing a URL WebKit can't serve. readFile gives us the raw bytes
      // and a reliable blob: URL instead.
      ...stubs
        .filter((s) => s.type === "video")
        .map(async (stub) => {
          let preview: string | undefined;
          try {
            const bytes = await readFile(stub.path);
            const blobSrc = URL.createObjectURL(new Blob([bytes], { type: stub.mimeType }));
            preview = (await captureVideoPoster(blobSrc))?.url;
            // Revoke the full-video blob URL — we only needed it for the poster.
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

  // ── File picker via Tauri dialog ──────────────────────────────────────────
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
      // Native file dialog steals focus and the webview doesn't restore
      // it — pull focus back to the chat input on every exit path
      // (success, cancel, or error) so the user can keep typing.
      textareaRef.current?.focus();
    }
  }, [attachments.length, maxAttachments, handlePaths, t]);

  // ── Paste (File objects, written to temp first) ───────────────────────────
  const handleBrowserFile = useCallback(async (file: File) => {
    if (attachments.length >= maxAttachments) { return; }
    // De-dupe by name+size — pasted files have no stable path.
    if (attachments.some((a) => a.name === file.name && a.size === file.size)) { return; }

    const id = `${Date.now()}-${Math.random()}`;
    const mime = file.type || mimeFromName(file.name);
    const type = typeFromMime(mime);
    const isImg = type === "image";
    const isVid = type === "video";

    // Image preview is available immediately from the File object.
    const preview = isImg ? URL.createObjectURL(file) : undefined;

    setAttachments((prev) => [
      ...prev,
      {
        id,
        path: "",
        name: file.name,
        size: file.size,
        mimeType: mime,
        preview,
        type,
        loading: true,
      },
    ]);

    // For videos, capture a poster frame concurrently with writeToTemp.
    let videoPoster: string | undefined;
    if (isVid) {
      const blobSrc = URL.createObjectURL(file);
      videoPoster = (await captureVideoPoster(blobSrc).catch(() => null))?.url;
      URL.revokeObjectURL(blobSrc);
    }

    const path = await writeToTemp(file).catch((err) => {
      console.error("[ChatInput] writeToTemp failed:", err);
      return null;
    });

    if (!path) {
      if (preview) { URL.revokeObjectURL(preview); }
      if (videoPoster) { URL.revokeObjectURL(videoPoster); }
      setAttachments((prev) => prev.filter((a) => a.id !== id));
      return;
    }

    setAttachments((prev) =>
      prev.map((a) => a.id === id
        ? { ...a, path, preview: preview ?? videoPoster, loading: false }
        : a)
    );
  }, [attachments, maxAttachments]);

  useImperativeHandle(ref, () => ({
    addFiles: (files: File[]) => { files.forEach(handleBrowserFile); },
    focus: () => { textareaRef.current?.focus(); },
  }), [handleBrowserFile]);

  // Register as the active file-drop target while mounted, so AppShell only
  // shows the drag overlay on views that can actually receive a file (not,
  // e.g., the voice/stream view where there's no input).
  useEffect(() => {
    const { register, unregister } = dropTargetStore;
    register();
    return unregister;
  }, []);

  // Global drop zone — AppShell fires this when an OS file drop lands while a
  // ChatInput is mounted.
  useEffect(() => {
    const handler = (e: Event) => {
      const paths: string[] = (e as CustomEvent<{ paths: string[] }>).detail.paths;
      handlePaths(paths);
    };
    window.addEventListener("pollis:pathdrop", handler);
    return () => window.removeEventListener("pollis:pathdrop", handler);
  }, [handlePaths]);

  useEffect(() => {
    if (textareaRef.current) {
      textareaRef.current.style.height = "auto";
      const maxH = 24 * 6;
      textareaRef.current.style.height = `${Math.min(textareaRef.current.scrollHeight, maxH)}px`;
    }
  }, [message]);

  const hasLoadingAttachments = attachments.some((a) => a.loading);

  // Live signal that the message, as typed, will ping every channel member.
  // Mirrors the backend's `mentions_all()` so the sender sees the hint exactly
  // when the send will fan out an `all_mention`.
  const willNotifyEveryone = canNotifyAll && mentionsAll(message);

  // ── @mention autocomplete (#843) ─────────────────────────────────────────
  // Candidates come from the roster of the conversation the user is already
  // in — never a directory lookup. See useMentionCandidates for why.
  const skin = useSkin();
  const mentionCandidates = useMentionCandidates();
  const [caret, setCaret] = useState(0);
  const [activeIndex, setActiveIndex] = useState(0);
  // Esc hides the suggestions for the current token; typing brings them back.
  const [mentionDismissed, setMentionDismissed] = useState(false);
  // Mirrors the textarea's scroll so the terminal ghost stays aligned once
  // the message wraps past the visible box.
  const [scrollTop, setScrollTop] = useState(0);
  // Where to put the caret after a completion is inserted. Applied in the
  // effect below, once React has committed the new value to the textarea.
  const pendingCaretRef = useRef<number | null>(null);

  const mentionQuery = mentionDismissed ? null : mentionQueryAt(message, caret);
  const suggestions = mentionQuery
    ? rankMentionCandidates(mentionCandidates, mentionQuery.query)
    : [];

  // Terminal skin: the ghost can only ever APPEND to what's typed, so it needs
  // a prefix match, and — like fish / zsh-autosuggestions — it is only offered
  // when the caret sits at the end of the line.
  const ghostTarget =
    skin === "terminal" && mentionQuery && caret === message.length
      ? suggestions.find((c) =>
          c.username.toLowerCase().startsWith(mentionQuery.query),
        )
      : undefined;
  const ghostText = ghostTarget
    ? ghostTarget.username.slice(mentionQuery!.query.length)
    : "";

  // Refined skin: the Slack-style list. Terminal never opens one.
  const showSuggestList = skin === "refined" && suggestions.length > 0;

  // Keep the highlight in range as the candidate list narrows on each keystroke.
  useEffect(() => {
    setActiveIndex((prev) => (prev < suggestions.length ? prev : 0));
  }, [suggestions.length]);

  useEffect(() => {
    const pending = pendingCaretRef.current;
    if (pending === null || !textareaRef.current) {
      return;
    }
    pendingCaretRef.current = null;
    textareaRef.current.setSelectionRange(pending, pending);
    textareaRef.current.focus();
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

  const handleSend = () => {
    if (!message.trim() && attachments.length === 0) { return; }
    if (hasLoadingAttachments) { return; }
    onSend(message.trim(), attachments);
    setMessage("");
    setDraft(draftKey, "");
    // Reset signals to "no longer typing" — covers the typing indicator
    // publisher in the parent so the receiver doesn't keep us in the
    // "still typing" state until TTL.
    onValueChange?.("");
    // Do NOT revoke preview blob URLs here — they may still be referenced by
    // optimistic message stubs in React Query cache. Let them be GC'd naturally.
    setAttachments([]);
    textareaRef.current?.focus();
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    // Mention keys are handled BEFORE the Enter-sends branch: while a
    // suggestion is on offer, Enter accepts it rather than sending a
    // half-typed name, exactly as Slack and Discord behave.
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

    // Terminal skin: Tab accepts the ghosted completion. There is nothing to
    // arrow through and nothing to dismiss beyond hiding the ghost.
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

    // Bash-history hand-off into the message log. "First line" is the first
    // LOGICAL line (no \n before the caret) — a soft-wrapped long first line
    // counts wholesale, which errs toward the REPL behavior over caret
    // movement, exactly like a shell with a wrapped prompt line.
    if (e.key === "ArrowUp" && onHistoryUp) {
      const el = e.currentTarget as HTMLTextAreaElement;
      const caret = el.selectionStart ?? 0;
      const collapsed = el.selectionEnd === caret;
      const onFirstLine = !message.slice(0, caret).includes("\n");
      if (collapsed && (message === "" || onFirstLine)) {
        if (onHistoryUp()) {
          e.preventDefault();
          el.blur();
          return;
        }
      }
    }

    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const handlePaste = useCallback((e: React.ClipboardEvent) => {
    // Screenshots and images copied from web content come through as
    // DataTransferItem files — handle these synchronously.
    const items = e.clipboardData.items;
    let hasFiles = false;
    for (let i = 0; i < items.length; i++) {
      if (items[i].kind === "file") {
        const file = items[i].getAsFile();
        if (file) { handleBrowserFile(file); hasFiles = true; }
      }
    }
    if (hasFiles) {
      e.preventDefault();
      return;
    }

    // For files copied from the OS file manager, WebKit doesn't expose the
    // clipboard data — go through the bridge to read it via the Rust OS
    // clipboard command. We don't prevent default here so normal text paste
    // still works alongside.
    readClipboardFiles().then((paths) => {
      if (paths.length > 0) {
        handlePaths(paths);
        return;
      }
      // WebKitGTK doesn't expose clipboard images as DataTransferItem files
      // the way macOS WebKit does, so screenshots / "copy image" from a
      // browser fall through to here. Fetch the raster image from the OS
      // clipboard, write it to temp, and import as an attachment.
      readClipboardImageToTemp().then((path) => {
        if (path) {
          handlePaths([path]);
        }
      }).catch(() => { /* no image on clipboard */ });
    }).catch(() => { /* clipboard unreadable */ });
  }, [handleBrowserFile, handlePaths]);

  const removeAttachment = useCallback((id: string) => {
    setAttachments((prev) => {
      const att = prev.find((a) => a.id === id);
      if (att?.preview) { URL.revokeObjectURL(att.preview); }
      return prev.filter((a) => a.id !== id);
    });
  }, []);

  return (
    <div
      className={`relative border-t border-line bg-bg ${className}`}
    >
      {/* Refined skin's mention list. Anchored to this container with
          `absolute`, never a portal or a fixed overlay. */}
      {showSuggestList && mentionQuery && (
        <MentionSuggestList
          candidates={suggestions}
          activeIndex={activeIndex}
          query={mentionQuery.query}
          onSelect={acceptMention}
          onHover={setActiveIndex}
        />
      )}

      {/* Attachment previews */}
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

      {/* @all hint — appears live while the message contains a standalone
          @all in a group channel, telling the sender the send will ping
          everyone. Not a modal: an inline row in the composer. */}
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

      {/* Input row — floor its height on the shared chrome-bar token so the
          composer, in-channel voice bar, and sidebar Close all match. */}
      <div className="flex items-start gap-1 px-2 py-1.5 min-h-bar">
        <button
          onClick={handlePickFiles}
          disabled={disabled || attachments.length >= maxAttachments}
          aria-label={t("composer.addAttachment")}
          className="pt-2 pb-1.5 px-1.5 flex-shrink-0 transition-colors text-muted enabled:hover:text-accent"
          style={{ opacity: disabled ? 0.4 : 1 }}
        >
          <Plus className="w-4 h-4" />
        </button>

        {/* Opens upward: the composer sits at the bottom of the window. The
            panel is in-flow within the composer, never a portal.

            `align="left"` — the panel's LEFT edge tracks this trigger, so it
            grows rightwards across the message area. The default (`right`)
            grows leftwards, and this trigger sits hard against the content
            region's left edge, which AppShell clips with `overflow: hidden` —
            so most of the panel was being cut off and left unclickable. */}
        {/* h-8 = the textarea's single-row box (1.5rem line + py-1), so the
            24px trigger centers on the first text line — vertically centered
            at minimum composer height, and pinned to the first line (like the
            +/send buttons) when the input grows. */}
        <EmojiPickerButton
          onSelect={insertAtCursor}
          placement="up"
          align="left"
          className="h-8 flex items-center"
        />

        {/* Positioning context for the terminal skin's inline ghost, which
            mirrors this exact box. */}
        <div className="relative flex-1 min-w-0">
          <textarea
            ref={textareaRef}
            data-testid="message-input"
            value={message}
            onChange={(e) => {
              const next = e.target.value;
              setMessage(next);
              setDraft(draftKey, next);
              onValueChange?.(next);
              setCaret(e.target.selectionStart ?? next.length);
              // Any edit re-opens suggestions that Esc had hidden.
              setMentionDismissed(false);
            }}
            onSelect={(e) => setCaret(e.currentTarget.selectionStart ?? 0)}
            onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
            onFocus={() => setIsFocused(true)}
            onBlur={() => setIsFocused(false)}
            onKeyDown={handleKeyDown}
            onPaste={handlePaste}
            placeholder={resolvedPlaceholder}
            disabled={disabled}
            autoFocus={autoFocus}
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
            spellCheck={false}
            rows={1}
            className={`chat-input-textarea w-full px-2 py-1 resize-none font-mono text-sm transition-colors border-0${isFocused ? " is-focused" : ""}`}
            style={{
              lineHeight: "1.5rem",
              minHeight: "1.5rem",
              borderRadius: "4px",
              background: isFocused ? "var(--c-accent)" : "var(--c-hover)",
              color: isFocused ? "var(--c-bg)" : "var(--c-text)",
              outline: "none",
              opacity: disabled ? 0.5 : 1,
            }}
            aria-label={t("composer.inputLabel")}
          />
          <MentionGhost
            value={message}
            ghost={ghostText}
            focused={isFocused}
            scrollTop={scrollTop}
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

      {/* Pre-send preview lightbox */}
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
