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
import { ChevronRight, Plus, X, Film, Music, Eye, EyeOff } from "lucide-react";
import { EmojiPickerButton } from "../Emoji/EmojiPickerButton";
import { getFileIcon, mimeFromName } from "../../utils/fileIcon";
import { blurhashFromUrl, captureVideoPoster } from "../../utils/imageProcessing";
import { SensitiveCover } from "../Media/SensitiveCover";
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
import { usePressActivation } from "../../hooks/usePressActivation";

/** Where a queued attachment's bytes are: a user's own file (never delete) or bytes staged in backend memory (no temp file). */
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
  // Marked sensitive (#1568): sent with `sens`, so receivers see it blurred.
  sensitive?: boolean;
  blurhash?: string;
}

export function stagedIdOf(attachment: Attachment): string | null {
  return attachment.source?.kind === "staged" ? attachment.source.id : null;
}

export interface ChatInputHandle {
  addFiles: (files: File[]) => void;
  focus: () => void;
  /** Put a failed draft back (#1497), only into an empty input / tray. */
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
  // Every keystroke; drives the typing indicator.
  onValueChange?: (value: string) => void;
  // Draft store key (`utils/drafts.ts`); pass null while the room id loads, re-syncs on change.
  draftKey?: string | null;
  // A standalone `@all` notifies everyone here (group channels only).
  canNotifyAll?: boolean;
  // ArrowUp on an empty/first line; return true to claim it.
  onHistoryUp?: () => boolean;
}

function typeFromMime(mime: string): Attachment["type"] {
  if (mime.startsWith("image/")) { return "image"; }
  if (mime.startsWith("video/")) { return "video"; }
  if (mime.startsWith("audio/")) { return "audio"; }
  return "file";
}

// Card size (#1568).
const PREVIEW_SIZE = 120;

const TOOLBAR_BUTTON =
  "flex h-6 w-6 items-center justify-center rounded bg-black/60 text-white hover:bg-black/80 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent";

const AttachmentPreview: React.FC<{
  attachment: Attachment;
  onRemove: (id: string) => void;
  onToggleSensitive: (id: string) => void;
  onExpand: (url: string, type: "image" | "video") => void;
}> = ({ attachment, onRemove, onToggleSensitive, onExpand }) => {
  const { t } = useTranslation("common");
  const hasVisualPreview = attachment.type === "image" || attachment.type === "video";
  const sensitive = attachment.sensitive === true;
  const canExpand = hasVisualPreview && !!attachment.preview && !attachment.loading && !sensitive;
  // Free to undo, so it acts on pointer-down (#1529).
  const expandPress = usePressActivation<HTMLDivElement>(
    canExpand ? () => onExpand(attachment.preview!, attachment.type as "image" | "video") : undefined,
    "press",
  );

  return (
    <div
      className="relative flex-shrink-0"
      style={{ width: PREVIEW_SIZE }}
      data-testid="composer-attachment"
      data-sensitive={sensitive ? "true" : "false"}
    >
      <div
        className="relative flex items-center justify-center overflow-hidden bg-surface-high"
        style={{
          width: PREVIEW_SIZE,
          height: PREVIEW_SIZE,
          borderRadius: 8,
          cursor: canExpand ? "zoom-in" : "default",
        }}
        {...expandPress}
      >
        {sensitive ? (
          <SensitiveCover blurhash={attachment.blurhash} />
        ) : attachment.loading ? (
          <span className="text-sm font-mono text-muted" style={{ animation: "pulse 1.5s ease-in-out infinite" }}>…</span>
        ) : attachment.preview ? (
          <img src={attachment.preview} alt={attachment.name} className="w-full h-full object-cover" style={{ borderRadius: 8 }} />
        ) : attachment.type === "video" ? (
          <Film size={32} className="text-dim" />
        ) : attachment.type === "audio" ? (
          <Music size={32} className="text-dim" />
        ) : (() => {
          const Icon = getFileIcon(attachment.name);
          return <Icon size={32} className="text-dim" />;
        })()}
      </div>
      <div
        className="mt-0.5 text-xs font-mono truncate text-muted"
        style={{ maxWidth: PREVIEW_SIZE }}
        title={attachment.name}
      >
        {attachment.name}
      </div>
      {/* The eye shows the current state. Pictures and videos only. */}
      <div className="absolute flex items-center gap-1" style={{ top: 4, insetInlineEnd: 4 }}>
        {hasVisualPreview && (
          <button
            type="button"
            data-testid="attachment-sensitive-toggle"
            onClick={() => onToggleSensitive(attachment.id)}
            aria-pressed={sensitive}
            aria-label={t(sensitive ? "composer.unmarkSensitive" : "composer.markSensitive", { name: attachment.name })}
            title={t(sensitive ? "composer.unmarkSensitive" : "composer.markSensitive", { name: attachment.name })}
            className={TOOLBAR_BUTTON}
          >
            {sensitive ? <EyeOff className="w-3.5 h-3.5" /> : <Eye className="w-3.5 h-3.5" />}
          </button>
        )}
        <button
          type="button"
          onClick={() => onRemove(attachment.id)}
          aria-label={t("composer.removeAttachment", { name: attachment.name })}
          className={TOOLBAR_BUTTON}
        >
          <X className="w-3.5 h-3.5" />
        </button>
      </div>
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
  const resolvedPlaceholder = placeholder ?? t("composer.placeholder");
  const [message, setMessage] = useState(() => getDraft(draftKey));
  // Re-sync on draftKey change during render, so no stale frame shows.
  const [prevDraftKey, setPrevDraftKey] = useState(draftKey);
  if (draftKey !== prevDraftKey) {
    setPrevDraftKey(draftKey);
    setMessage(getDraft(draftKey));
  }
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [isFocused, setIsFocused] = useState(false);
  const [expandedPreview, setExpandedPreview] = useState<{ url: string; type: "image" | "video" } | null>(null);
  const inputRef = useRef<RichTextInputHandle>(null);

  // Splice at the caret; only the input knows where it was (contentEditable loses it on blur).
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

  const prevExpandedPreviewRef = useRef(expandedPreview);
  useEffect(() => {
    if (prevExpandedPreviewRef.current && !expandedPreview) {
      inputRef.current?.focus();
    }
    prevExpandedPreviewRef.current = expandedPreview;
  }, [expandedPreview]);

  // Refocus on any attachment count change (not on mount).
  const prevAttachmentLengthRef = useRef(attachments.length);
  useEffect(() => {
    if (prevAttachmentLengthRef.current !== attachments.length) {
      inputRef.current?.focus();
    }
    prevAttachmentLengthRef.current = attachments.length;
  }, [attachments.length]);

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
          return p;
        }
      })
    );
    const toProcess = checks.filter((p): p is string => p !== null);
    if (toProcess.length === 0) { return; }

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
          }
          setAttachments((prev) =>
            prev.map((a) => a.id === stub.id ? { ...a, preview, loading: false } : a)
          );
        }),
      // Not convertFileSrc: it percent-encodes Linux paths WebKit can't serve.
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

  const handleBrowserFile = useCallback(async (file: File) => {
    if (attachments.length >= maxAttachments) { return; }
    // De-dupe by name+size: pasted files have no path.
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
      // Removed while in flight: release the bytes.
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

  // Rebuilt every render, since `handleSend` reads the latest state.
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

  // AppShell shows the drag overlay only where a file can be received.
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

  const willNotifyEveryone = canNotifyAll && mentionsAll(message);

  // @mention autocomplete (#843): candidates from this conversation's roster only.
  const skin = useSkin();
  const mentionCandidates = useMentionCandidates();
  // Async `selectionchange` caret; the ArrowUp hand-off reads the selection directly.
  const [caret, setCaret] = useState(0);
  const [activeIndex, setActiveIndex] = useState(0);
  const [mentionDismissed, setMentionDismissed] = useState(false);
  const pendingCaretRef = useRef<number | null>(null);

  const mentionQuery = mentionDismissed ? null : mentionQueryAt(message, caret);
  const suggestions = mentionQuery
    ? rankMentionCandidates(mentionCandidates, mentionQuery.query)
    : [];

  // Terminal ghost: prefix match, only with the caret at line end.
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

  // :shortcode: autocomplete. Mentions take precedence for Enter/Tab/Esc.
  const [emojiDismissed, setEmojiDismissed] = useState(false);
  const emojiQuery =
    emojiDismissed || mentionQuery ? null : shortcodeQueryAt(message, caret);
  // Loaded on focus, so `:tada:` resolves on the first try without joining the startup chunk (#874).
  const shortcodeEntries = useShortcodeEntries(isFocused || emojiQuery !== null);
  const emojiSuggestions = emojiQuery
    ? rankShortcodeEntries(shortcodeEntries, emojiQuery.query)
    : [];

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
    // Passive effect: runs after RichTextInput's layout effect rebuilt the projection.
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

  // List/ghost accept adds a trailing space; typing the closing ':' does not.
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
    onValueChange?.("");
    // Don't revoke previews: optimistic stubs may still reference them.
    setAttachments([]);
    inputRef.current?.focus();
  };
  sendRef.current = handleSend;

  const handleKeyDown = (e: React.KeyboardEvent) => {
    // Before Enter-sends: Enter accepts a suggestion.
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

    // Hand off to the log on the first logical line. Read the selection synchronously: `selectionchange` is async.
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

    // WebKit hides OS file-manager copies; read them via the Rust clipboard command.
    readClipboardFiles().then((paths) => {
      if (paths.length > 0) {
        handlePaths(paths);
        return;
      }
      // WebKitGTK doesn't expose clipboard images as files; read PNG bytes from the OS.
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

  const toggleSensitive = useCallback((id: string) => {
    setAttachments((prev) => prev.map((a) => (a.id === id ? { ...a, sensitive: !a.sensitive } : a)));
  }, []);

  // Blurhash for a marked attachment's cover, computed once.
  const blurhashAttemptedRef = useRef(new Set<string>());
  useEffect(() => {
    for (const att of attachments) {
      if (!att.sensitive || att.blurhash || !att.preview || blurhashAttemptedRef.current.has(att.id)) {
        continue;
      }
      blurhashAttemptedRef.current.add(att.id);
      void blurhashFromUrl(att.preview).then((meta) => {
        if (!meta) {
          return;
        }
        setAttachments((prev) => prev.map((a) => (a.id === att.id ? { ...a, blurhash: meta.hash } : a)));
      });
    }
  }, [attachments]);

  const removeAttachment = useCallback((id: string) => {
    setAttachments((prev) => {
      const att = prev.find((a) => a.id === id);
      if (att?.preview) { URL.revokeObjectURL(att.preview); }
      const stagedId = att ? stagedIdOf(att) : null;
      if (stagedId) { void discardStagedAttachment(stagedId); }
      return prev.filter((a) => a.id !== id);
    });
  }, []);

  // Release whatever is still staged on unmount; via a ref so it runs only then.
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
      {/* Anchored with `absolute`, never a portal. */}
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
              onToggleSensitive={toggleSensitive}
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

      {/* Single-line height is `--composer-h`, shared with the Close bars so borders align. */}
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

        {/* align="left": AppShell clips the content region's left edge. */}
        <EmojiPickerButton
          onSelect={insertAtCursor}
          placement="up"
          align="left"
          className="h-8 flex items-center"
        />

        <div className="relative flex-1 min-w-0">
          <RichTextInput
            ref={inputRef}
            testId="message-input"
            value={message}
            onChange={(next, nextCaret) => {
              setMentionDismissed(false);
              setEmojiDismissed(false);
              // Slack-style direct substitution on typing the closing ':' (single-char inserts only).
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
