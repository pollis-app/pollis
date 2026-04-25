import React, { useState, useRef, useCallback, useEffect, useImperativeHandle } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { writeFile, readFile, stat } from "@tauri-apps/plugin-fs";
import { tempDir } from "@tauri-apps/api/path";
import { invoke } from "@tauri-apps/api/core";
import { ChevronRight, Plus, X, Film, Music } from "lucide-react";
import { getFileIcon } from "../../utils/fileIcon";

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
}

const formatFileSize = (bytes: number): string => {
  if (bytes === 0) { return ""; }
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB"];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + sizes[i];
};

function mimeFromName(name: string): string {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  const map: Record<string, string> = {
    jpg: "image/jpeg", jpeg: "image/jpeg", png: "image/png",
    gif: "image/gif", webp: "image/webp", svg: "image/svg+xml",
    mp4: "video/mp4", mov: "video/quicktime", webm: "video/webm",
    mp3: "audio/mpeg", wav: "audio/wav", ogg: "audio/ogg", m4a: "audio/mp4",
    flac: "audio/flac", opus: "audio/opus", aac: "audio/aac",
    pdf: "application/pdf", zip: "application/zip",
  };
  return map[ext] ?? "application/octet-stream";
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

/// Capture a poster frame from a video src URL.
/// Returns a blob URL for a JPEG thumbnail, or undefined on failure.
async function generateVideoPoster(src: string): Promise<string | undefined> {
  return new Promise((resolve) => {
    const vid = document.createElement("video");
    vid.muted = true;
    vid.playsInline = true;
    vid.preload = "metadata";

    let resolved = false;
    const finish = (url?: string) => {
      if (resolved) { return; }
      resolved = true;
      vid.src = "";
      resolve(url);
    };

    vid.addEventListener("loadedmetadata", () => {
      vid.currentTime = Math.min(0.5, vid.duration > 0 ? vid.duration * 0.1 : 0.5);
    }, { once: true });

    vid.addEventListener("seeked", () => {
      const canvas = document.createElement("canvas");
      // Cap to 1280px to stay well within WebKit/GDK's native surface limits.
      const MAX_DIM = 1280;
      let cw = vid.videoWidth || 320;
      let ch = vid.videoHeight || 180;
      if (cw > MAX_DIM) { ch = Math.round(ch * MAX_DIM / cw); cw = MAX_DIM; }
      if (ch > MAX_DIM) { cw = Math.round(cw * MAX_DIM / ch); ch = MAX_DIM; }
      canvas.width = cw;
      canvas.height = ch;
      const ctx = canvas.getContext("2d");
      if (ctx) {
        ctx.drawImage(vid, 0, 0, cw, ch);
        canvas.toBlob((blob) => {
          finish(blob ? URL.createObjectURL(blob) : undefined);
        }, "image/jpeg", 0.75);
      } else {
        finish(undefined);
      }
    }, { once: true });

    vid.addEventListener("error", () => { finish(undefined); }, { once: true });

    // Timeout guard — if nothing fires after 5s, give up.
    setTimeout(() => { finish(undefined); }, 5000);

    vid.src = src;
    vid.load();
  });
}

const PREVIEW_SIZE = 80;

const AttachmentPreview: React.FC<{
  attachment: Attachment;
  onRemove: (id: string) => void;
  onExpand: (url: string, type: "image" | "video") => void;
}> = ({ attachment, onRemove, onExpand }) => {
  const hasVisualPreview = attachment.type === "image" || attachment.type === "video";
  const canExpand = hasVisualPreview && !!attachment.preview && !attachment.loading;

  return (
    <div className="relative flex-shrink-0" style={{ width: PREVIEW_SIZE }}>
      <div
        className="flex items-center justify-center overflow-hidden"
        style={{
          width: PREVIEW_SIZE,
          height: PREVIEW_SIZE,
          border: "2px solid var(--c-border)",
          borderRadius: 8,
          background: "var(--c-surface-high)",
          cursor: canExpand ? "zoom-in" : "default",
        }}
        onClick={() => {
          if (canExpand) {
            onExpand(attachment.preview!, attachment.type as "image" | "video");
          }
        }}
      >
        {attachment.loading ? (
          <span className="text-sm font-mono" style={{ color: "var(--c-text-muted)", animation: "pulse 1.5s ease-in-out infinite" }}>…</span>
        ) : attachment.preview ? (
          <img src={attachment.preview} alt={attachment.name} className="w-full h-full object-cover" style={{ borderRadius: 6 }} />
        ) : attachment.type === "video" ? (
          <Film size={28} style={{ color: "var(--c-text-dim)" }} />
        ) : attachment.type === "audio" ? (
          <Music size={28} style={{ color: "var(--c-text-dim)" }} />
        ) : (() => {
          const Icon = getFileIcon(attachment.name);
          return <Icon size={28} style={{ color: "var(--c-text-dim)" }} />;
        })()}
      </div>
      <div
        className="mt-0.5 text-xs font-mono truncate"
        style={{ color: "var(--c-text-muted)", maxWidth: PREVIEW_SIZE }}
        title={attachment.name}
      >
        {attachment.name}
      </div>
      <button
        onClick={() => onRemove(attachment.id)}
        aria-label={`Remove ${attachment.name}`}
        className="absolute flex items-center justify-center"
        style={{
          top: -6,
          right: -6,
          width: 22,
          height: 22,
          borderRadius: 4,
          background: "var(--c-surface-high)",
          border: "1px solid var(--c-border-active)",
          color: "var(--c-text-dim)",
        }}
      >
        <X className="w-3.5 h-3.5" />
      </button>
    </div>
  );
};

export const ChatInput = React.forwardRef<ChatInputHandle, ChatInputProps>(({
  onSend,
  placeholder = "Type a message…",
  disabled = false,
  autoFocus = false,
  className = "",
  maxAttachments = 10,
}, ref) => {
  const [message, setMessage] = useState("");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [isFocused, setIsFocused] = useState(false);
  // Lightbox for previewing attachments before send.
  const [expandedPreview, setExpandedPreview] = useState<{ url: string; type: "image" | "video" } | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

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
            preview = await generateVideoPoster(blobSrc);
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
    const result = await open({
      multiple: true,
      directory: false,
      title: "Add files",
    }).catch((err) => { console.error("[ChatInput] open dialog failed:", err); return null; });
    if (!result) { return; }
    await handlePaths(Array.isArray(result) ? result : [result]);
  }, [attachments.length, maxAttachments, handlePaths]);

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
      videoPoster = await generateVideoPoster(blobSrc).catch(() => undefined);
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

  // Global drop zone — AppShell fires this when Tauri intercepts an OS file drop.
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

  const handleSend = () => {
    if (!message.trim() && attachments.length === 0) { return; }
    if (hasLoadingAttachments) { return; }
    onSend(message.trim(), attachments);
    setMessage("");
    // Do NOT revoke preview blob URLs here — they may still be referenced by
    // optimistic message stubs in React Query cache. Let them be GC'd naturally.
    setAttachments([]);
    textareaRef.current?.focus();
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
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
    // clipboard data — invoke Rust to read it directly via the OS clipboard API.
    // We don't prevent default here so normal text paste still works alongside.
    invoke<string[]>("read_clipboard_files").then((paths) => {
      if (paths.length > 0) {
        handlePaths(paths);
        return;
      }
      // WebKitGTK doesn't expose clipboard images as DataTransferItem files
      // the way macOS WebKit does, so screenshots / "copy image" from a
      // browser fall through to here. Fetch the raster image from the OS
      // clipboard via Rust, write it to temp, and import as an attachment.
      invoke<string>("read_clipboard_image_to_temp").then((path) => {
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
      className={`border-t ${className}`}
      style={{ borderColor: "var(--c-border)", background: "var(--c-bg)" }}
    >

      {/* Attachment previews */}
      {attachments.length > 0 && (
        <div
          className="px-2 py-2 flex items-start gap-2 flex-wrap"
          style={{ borderBottom: "1px solid var(--c-border)" }}
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

      {/* Input row */}
      <div className="flex items-start gap-1 px-2 py-1.5">
        <button
          onClick={handlePickFiles}
          disabled={disabled || attachments.length >= maxAttachments}
          aria-label="Add attachment"
          className="p-1.5 flex-shrink-0 transition-colors"
          style={{ color: "var(--c-text-muted)", opacity: disabled ? 0.4 : 1 }}
          onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-accent)"; }}
          onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-text-muted)"; }}
        >
          <Plus className="w-4 h-4" />
        </button>

        <textarea
          ref={textareaRef}
          data-testid="message-input"
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          onFocus={() => setIsFocused(true)}
          onBlur={() => setIsFocused(false)}
          onKeyDown={handleKeyDown}
          onPaste={handlePaste}
          placeholder={placeholder}
          disabled={disabled}
          autoFocus={autoFocus}
          autoComplete="off"
          autoCorrect="off"
          autoCapitalize="off"
          spellCheck={false}
          rows={1}
          className="chat-input-textarea flex-1 min-w-0 px-2 py-1 resize-none font-mono text-sm transition-colors"
          style={{
            lineHeight: "1.5rem",
            minHeight: "1.5rem",
            borderRadius: "4px",
            background: isFocused ? "var(--c-accent)" : "var(--c-hover)",
            color: isFocused ? "var(--c-bg)" : "var(--c-text)",
            outline: "none",
            border: "none",
            opacity: disabled ? 0.5 : 1,
          }}
          aria-label="Message input"
        />

        <button
          onClick={handleSend}
          disabled={disabled || (!message.trim() && attachments.length === 0) || hasLoadingAttachments}
          data-testid="message-send-button"
          aria-label="Send message"
          className="p-1.5 flex-shrink-0 transition-colors"
          style={{
            color: "var(--c-text-muted)",
            opacity: disabled || (!message.trim() && !attachments.length) ? 0.3 : 1,
          }}
          onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-accent)"; }}
          onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-text-muted)"; }}
        >
          <ChevronRight className="w-4 h-4" />
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
              alt="Preview"
              style={{ maxWidth: "90vw", maxHeight: "85vh", objectFit: "contain", cursor: "default", borderRadius: "1rem" }}
              onClick={(e) => e.stopPropagation()}
            />
          )}
          <button
            onClick={() => setExpandedPreview(null)}
            className="mt-3 text-xs font-mono focus:outline-none focus:ring-2 focus:ring-[var(--c-accent)] focus:ring-offset-1 focus:ring-offset-black px-2 py-0.5"
            style={{ color: "var(--c-text-dim)", background: "none", border: "1px solid transparent", borderRadius: 4, cursor: "pointer" }}
            onMouseEnter={(e) => {
              (e.currentTarget as HTMLElement).style.background = "var(--c-accent)";
              (e.currentTarget as HTMLElement).style.color = "black";
            }}
            onMouseLeave={(e) => {
              (e.currentTarget as HTMLElement).style.background = "none";
              (e.currentTarget as HTMLElement).style.color = "var(--c-text-dim)";
            }}
          >
            [esc]
          </button>
        </div>
      )}
    </div>
  );
});
