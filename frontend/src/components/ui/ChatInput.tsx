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

/**
 * Where the bytes of a queued attachment are, and the only two answers there
 * are.
 *
 * A discriminated union rather than an optional `path`, because the two cases
 * are not the same thing wearing different values:
 *
 * - `path` is a file the USER already has — a picker selection or an OS drag
 *   and drop. This app did not create it and must never delete it.
 * - `staged` is bytes the webview handed us with no path of its own — a paste,
 *   or a drop the webview surfaced as a `File`. They live in the backend's
 *   memory (`pollis_core::commands::staging`).
 *
 * This used to be one `path` field for both, because the second case wrote the
 * bytes to the OS temp directory under the file's original name to manufacture
 * one. Nothing deleted those files, so the plaintext of every file a user had
 * ever pasted was still sitting in `/tmp`. With no path there is nothing to
 * forget to delete.
 */
export type AttachmentSource =
  | { kind: "path"; path: string }
  | { kind: "staged"; id: string };

export interface Attachment {
  id: string;
  // null only while the source is still being prepared (`loading`).
  source: AttachmentSource | null;
  name: string;
  size: number;      // bytes (0 if unknown)
  mimeType: string;
  preview?: string;  // blob URL for image/video poster previews
  type: "image" | "video" | "audio" | "file";
  loading?: boolean; // true while source/preview is still being prepared
}

/** The staged handle of an attachment, or null when its bytes are a real file. */
export function stagedIdOf(attachment: Attachment): string | null {
  return attachment.source?.kind === "staged" ? attachment.source.id : null;
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
  const inputRef = useRef<RichTextInputHandle>(null);

  // Insert emoji text at the caret, not at the end — appending would be wrong
  // the moment someone picks an emoji mid-sentence, which is the common case.
  //
  // The splice itself lives in the input, which is the only thing that knows
  // where the caret was: a `<textarea>` kept `selectionStart` across the blur
  // the picker click causes, and a `contentEditable` does not. The resulting
  // `onChange` runs the same draft / typing-indicator writes as a keystroke.
  const insertAtCursor = useCallback((text: string) => {
    inputRef.current?.insertText(text);
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
      inputRef.current?.focus();
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
      inputRef.current?.focus();
    }
    prevAttachmentLengthRef.current = attachments.length;
  }, [attachments.length]);

  // ── Shared path-based attachment builder (picker + OS drag-drop) ─────────
  const handlePaths = useCallback(async (paths: string[]) => {
    // De-dupe against already-queued paths.
    const existingPaths = new Set(
      attachments
        .map((a) => (a.source?.kind === "path" ? a.source.path : null))
        .filter((p): p is string => p !== null),
    );
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

    // Add stubs immediately so the user sees cards right away. Typed with the
    // narrowed source so the preview loaders below can read `source.path`
    // without re-checking a discriminant this function just set.
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
            const bytes = await readFile(stub.source.path);
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
            const bytes = await readFile(stub.source.path);
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
      inputRef.current?.focus();
    }
  }, [attachments.length, maxAttachments, handlePaths, t]);

  // ── Paste (File objects, staged in the backend's memory) ──────────────────
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
        source: null,
        name: file.name,
        size: file.size,
        mimeType: mime,
        preview,
        type,
        loading: true,
      },
    ]);

    // For videos, capture a poster frame concurrently with the staging call.
    let videoPoster: string | undefined;
    if (isVid) {
      const blobSrc = URL.createObjectURL(file);
      videoPoster = (await captureVideoPoster(blobSrc).catch(() => null))?.url;
      URL.revokeObjectURL(blobSrc);
    }

    // Hand the bytes to the backend. This is the call that used to write the
    // file into the OS temp directory under its own name and leave it there
    // forever; the bytes now sit in the backend's memory and the only handle
    // to them is an opaque id.
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
      // The card was removed while its bytes were in flight — release them
      // rather than leaving an entry nothing will ever upload or discard.
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

  useImperativeHandle(ref, () => ({
    addFiles: (files: File[]) => { files.forEach(handleBrowserFile); },
    focus: () => { inputRef.current?.focus(); },
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

  // The auto-grow effect that used to measure `scrollHeight` on every keystroke
  // is gone: a `contentEditable` sizes to its content on its own, so the clamp
  // is `min-height` / `max-height` on the element (see the input row below) and
  // the six-line ceiling is the same six lines it always was.

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
  // Where the completion tracks think the caret is. Fed by the input's
  // `selectionchange`, which is asynchronous — fine for deciding whether a
  // suggestion list is open, and deliberately NOT what the ArrowUp hand-off
  // reads (see `handleKeyDown`).
  const [caret, setCaret] = useState(0);
  const [activeIndex, setActiveIndex] = useState(0);
  // Esc hides the suggestions for the current token; typing brings them back.
  const [mentionDismissed, setMentionDismissed] = useState(false);
  // Where to put the caret after a completion is inserted. Applied in the
  // effect below, once React has committed the new value to the input.
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

  // ── :shortcode: autocomplete and substitution ────────────────────────────
  //
  // PRECEDENCE: the composer now has two completion tracks, and exactly one
  // may claim Enter / Tab / Esc at a time. Mentions win — they are tested
  // first here and first in `handleKeyDown`, and an open mention query
  // suppresses the emoji one outright.
  //
  // In practice the two cannot both be open: a query opens on '@' or ':' at
  // the start of a word, and neither body alphabet contains the other's
  // trigger character, so no caret position sits inside both. The ordering is
  // stated anyway so that widening either alphabet has one obvious place to be
  // reasoned about rather than two handlers quietly fighting over a key.
  const [emojiDismissed, setEmojiDismissed] = useState(false);
  const emojiQuery =
    emojiDismissed || mentionQuery ? null : shortcodeQueryAt(message, caret);
  // Loaded while the composer is focused rather than on the first colon: the
  // standard half arrives over a dynamic import, and `:tada:` typed straight
  // out has to resolve to the group's own emoji on the FIRST try, not on the
  // second once a chunk landed. Focus is still after first paint, so the table
  // stays out of the startup chunk exactly as #874 requires.
  const shortcodeEntries = useShortcodeEntries(isFocused || emojiQuery !== null);
  const emojiSuggestions = emojiQuery
    ? rankShortcodeEntries(shortcodeEntries, emojiQuery.query)
    : [];

  // Terminal skin: same rule as the mention ghost — a prefix match, offered
  // only with the caret at the end of the line. The closing ':' is part of the
  // ghost so what is shown is the `:joy:` that typing it out would produce.
  const emojiGhostTarget =
    skin === "terminal" && emojiQuery && caret === message.length
      ? emojiSuggestions.find((e) => e.shortcode.startsWith(emojiQuery.query))
      : undefined;
  const emojiGhostText = emojiGhostTarget
    ? `${emojiGhostTarget.shortcode.slice(emojiQuery!.query.length)}:`
    : "";

  const showEmojiList = skin === "refined" && emojiSuggestions.length > 0;

  // One highlight for both lists, because only one is ever open.
  const activeListLength = showSuggestList ? suggestions.length : emojiSuggestions.length;

  // Keep the highlight in range as the candidate list narrows on each keystroke.
  useEffect(() => {
    setActiveIndex((prev) => (prev < activeListLength ? prev : 0));
  }, [activeListLength]);

  useEffect(() => {
    const pending = pendingCaretRef.current;
    if (pending === null || !inputRef.current) {
      return;
    }
    pendingCaretRef.current = null;
    // A passive effect, so it runs after RichTextInput's layout effect has
    // rebuilt the projection — the offset it is given is meaningless against
    // the previous DOM.
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

  // Accepting from the list or the ghost writes the emoji plus a trailing
  // space: the word is finished, and the user carries on typing. Typing the
  // closing ':' yourself does NOT add one — see the substitution path in
  // `onChange` — because that happens mid-word.
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
    // Reset signals to "no longer typing" — covers the typing indicator
    // publisher in the parent so the receiver doesn't keep us in the
    // "still typing" state until TTL.
    onValueChange?.("");
    // Do NOT revoke preview blob URLs here — they may still be referenced by
    // optimistic message stubs in React Query cache. Let them be GC'd naturally.
    setAttachments([]);
    inputRef.current?.focus();
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

    // `:shortcode:` list — the same key contract as the mention list, and
    // reached only when no mention query is open (see the precedence note
    // where `emojiQuery` is computed).
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

    // Terminal skin: Tab accepts the ghosted shortcode.
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

    // Bash-history hand-off into the message log. "First line" is the first
    // LOGICAL line (no \n before the caret) — a soft-wrapped long first line
    // counts wholesale, which errs toward the REPL behavior over caret
    // movement, exactly like a shell with a wrapped prompt line.
    //
    // The selection is read from the input SYNCHRONOUSLY rather than off the
    // `caret` state: a contentEditable reports caret moves through
    // `selectionchange`, which is asynchronous, so two ArrowUps in quick
    // succession would judge the second against the first one's stale offset
    // and refuse to hand off. `<textarea>` had `selectionStart` for this.
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

  // Returns true when the paste was a file and the text path must not run.
  // RichTextInput has already prevented the browser's own (HTML) paste by the
  // time this is called, so all this decides is whether plain text follows.
  const handlePaste = useCallback((e: React.ClipboardEvent): boolean => {
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
      return true;
    }

    // For files copied from the OS file manager, WebKit doesn't expose the
    // clipboard data — go through the bridge to read it via the Rust OS
    // clipboard command. This probe is async and does not claim the paste, so
    // the text half is inserted alongside exactly as it was before.
    readClipboardFiles().then((paths) => {
      if (paths.length > 0) {
        handlePaths(paths);
        return;
      }
      // WebKitGTK doesn't expose clipboard images as DataTransferItem files
      // the way macOS WebKit does, so screenshots / "copy image" from a
      // browser fall through to here. Fetch the raster image from the OS
      // clipboard as PNG bytes and import it on the same staging path a
      // pasted `File` takes — no temp file on either route.
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
      // Release the bytes, not just the card. The old code revoked the preview
      // blob URL and dropped the array entry, and left the file it had written
      // to the OS temp directory exactly where it was.
      const stagedId = att ? stagedIdOf(att) : null;
      if (stagedId) { void discardStagedAttachment(stagedId); }
      return prev.filter((a) => a.id !== id);
    });
  }, []);

  // Unmount with attachments still queued — navigating away mid-compose,
  // switching channels, closing a thread. Whatever is staged goes with the
  // composer that staged it.
  //
  // Reads through a ref so the effect can depend on nothing and therefore run
  // only on unmount; a dependency on `attachments` would fire this on every
  // keystroke that changed the list and discard bytes still in use.
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

      {/* Refined skin's `:shortcode:` list. Same anchoring, and it never
          co-exists with the mention list — the two queries are mutually
          exclusive. */}
      {showEmojiList && emojiQuery && (
        <EmojiSuggestList
          entries={emojiSuggestions}
          activeIndex={activeIndex}
          query={emojiQuery.query}
          onSelect={acceptEmoji}
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

        {/* The composer's field. The ghost lives INSIDE it now (see
            RichTextInput), so this box is no longer a positioning context for
            an absolutely-positioned mirror — the mirror is gone.

            At most one completion track ever offers a ghost (the two queries
            are mutually exclusive, see the precedence note above), so the one
            ghost slot carries both and keeps each track's own testid. */}
        <div className="relative flex-1 min-w-0">
          <RichTextInput
            ref={inputRef}
            testId="message-input"
            value={message}
            onChange={(next, nextCaret) => {
              // Any edit re-opens suggestions that Esc had hidden.
              setMentionDismissed(false);
              setEmojiDismissed(false);
              // Slack's direct substitution: the moment the user types the ':'
              // that CLOSES a shortcode they know by heart, the emoji replaces
              // the whole `:name:` and no list is involved. Gated on a
              // single-character insertion so a paste that happens to end in a
              // colon is left exactly as it was pasted.
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
              // Six lines, the same ceiling the measured auto-grow enforced —
              // in rem now, so it tracks the user's font size instead of
              // assuming a 16px root.
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
