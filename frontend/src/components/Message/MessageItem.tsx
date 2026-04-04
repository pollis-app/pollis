import React, { useState, useEffect, useRef } from "react";
import { decode } from "blurhash";
import { Reply, Download, Film, File as FileIcon, Music } from "lucide-react";
import { useAppStore } from "../../stores/appStore";
import { downloadAndDecryptMedia } from "../../services/r2-upload";
import { LinkifiedText } from "../ui/LinkifiedText";
import { LoadingSpinner } from "../ui/LoaderSpinner";
// import { MessageReactions } from "./MessageReactions";
import type { Message, MessageAttachment } from "../../types";

interface MessageItemProps {
  message: Message;
  allMessages?: Message[];
  authorUsername?: string;
  onReply?: (messageId: string) => void;
  onPin?: (messageId: string) => void;
  onScrollToReply?: (messageId: string) => void;
}

const formatTimestamp = (timestamp: number): string => {
  const tsMs = timestamp < 1e12 ? timestamp * 1000 : timestamp;
  const d = new Date(tsMs);
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  return `${hh}:${mm}`;
};

export const MessageItem: React.FC<MessageItemProps> = ({
  message,
  allMessages = [],
  authorUsername = "unknown",
  onReply,
  onScrollToReply,
}) => {
  const { currentUser } = useAppStore();
  const isOwn = message.sender_id === currentUser?.id;

  const replyTo = message.reply_to_message_id
    ? allMessages.find((m) => m.id === message.reply_to_message_id)
    : null;

  const replyToAuthor = replyTo
    ? (replyTo.sender_username ?? replyTo.sender_id)
    : null;

  // content_decrypted is undefined when decryption failed (the server returned
  // null). Show [encrypted] in that case rather than an empty row.
  const content = message.content_decrypted ?? "[encrypted]";

  // Sort attachments: images and videos first, then everything else.
  const sortedAttachments = message.attachments && message.attachments.length > 0
    ? [...message.attachments].sort((a, b) => {
        const rank = (ct: string) => ct.startsWith("image/") || ct.startsWith("video/") ? 0 : 1;
        return rank(a.content_type) - rank(b.content_type);
      })
    : null;

  return (
    <div
      data-testid={`message-${message.id}`}
      aria-label={`Message from ${authorUsername}`}
      className="group relative px-3 py-1 mb-0.5 hover:bg-[var(--c-hover)] transition-colors duration-75"
    >
      {/* Reply thread indicator */}
      {replyTo && (
        <button
          data-testid={`reply-preview-${message.reply_to_message_id}`}
          onClick={() => onScrollToReply?.(message.reply_to_message_id!)}
          className="flex items-center gap-1 text-xs font-mono mb-0.5 pl-14 opacity-60 hover:opacity-90 transition-opacity"
          style={{ color: "var(--c-text-muted)" }}
        >
          <Reply size={10} style={{ transform: "scaleX(-1)" }} />
          {replyToAuthor && (
            <span className="font-semibold flex-shrink-0" style={{ color: "var(--c-text-dim)" }}>
              {replyToAuthor}:
            </span>
          )}
          <span className="truncate max-w-xs">
            {replyTo.content_decrypted?.slice(0, 80) || "[encrypted]"}
          </span>
        </button>
      )}

      {/* IRC-style inline row: HH:MM  username  message */}
      <div className="flex items-baseline gap-0 min-w-0">
        <span
          data-testid="message-timestamp"
          className="flex-shrink-0 text-sm font-mono tabular-nums select-none w-12 mr-2"
          style={{ color: "var(--c-text-muted)" }}
        >
          {formatTimestamp(message.created_at)}
        </span>

        <span
          data-testid="message-author"
          className="flex-shrink-0 font-mono text-sm font-semibold mr-1"
          style={{
            color: isOwn ? "var(--c-accent)" : "var(--c-text-dim)",
          }}
        >
          {authorUsername}
        </span>

        <span
          className="font-mono text-sm select-none mr-1 flex-shrink-0"
          style={{ color: "var(--c-text-muted)" }}
          aria-hidden="true"
        >
          {":"}
        </span>

        <span
          data-testid="message-content"
          className="font-mono text-sm break-words flex-1 min-w-0"
          style={{ color: "var(--c-text)", whiteSpace: "pre-wrap" }}
        >
          <LinkifiedText text={content} />
          {message.status && message.status !== "sent" && (
            <span className="ml-1 text-xs" style={{ color: "var(--c-text-muted)" }}>
              [{message.status}]
            </span>
          )}
        </span>

        {/* Reply button — only visible on hover */}
        <button
          data-testid="reply-button"
          onClick={() => onReply?.(message.id)}
          aria-label="Reply"
          className="flex-shrink-0 ml-1 opacity-0 group-hover:opacity-60 hover:!opacity-100 transition-opacity"
          style={{ color: "var(--c-text-muted)" }}
        >
          <Reply size={12} />
        </button>
      </div>

      {/* Attachments — row layout, media sorted first */}
      {sortedAttachments && (
        <div className="mt-1 flex flex-row flex-wrap gap-2" style={{ alignItems: "flex-start" }}>
          {sortedAttachments.map((a) => (
            <AttachmentDisplay key={a.id} attachment={a} />
          ))}
        </div>
      )}

      {/* Reactions row — disabled, needs more thought */}
      {/* <MessageReactions messageId={message.id} /> */}
    </div>
  );
};

// Co-located: only used by AttachmentDisplay.
const BlurhashCanvas: React.FC<{ hash: string; width: number; height: number }> = ({
  hash,
  width,
  height,
}) => {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) { return; }
    try {
      const pixels = decode(hash, 32, 32);
      const ctx = canvas.getContext("2d");
      if (!ctx) { return; }
      const imageData = ctx.createImageData(32, 32);
      imageData.data.set(pixels);
      ctx.putImageData(imageData, 0, 0);
    } catch {
      // Invalid or unsupported blurhash — canvas stays blank, no crash.
    }
  }, [hash]);

  // Aspect-ratio-correct container height (capped at 200px).
  const aspect = width > 0 && height > 0 ? height / width : 1;
  const containerH = Math.min(Math.round(280 * aspect), 200);

  return (
    <div style={{ position: "relative", width: "100%", height: containerH, overflow: "hidden" }}>
      <canvas
        ref={canvasRef}
        width={32}
        height={32}
        style={{
          position: "absolute",
          inset: 0,
          width: "100%",
          height: "100%",
          filter: "blur(6px)",
          transform: "scale(1.08)",
        }}
      />
    </div>
  );
};

const formatFileSize = (bytes: number) => {
  if (bytes === 0) { return ""; }
  const sizes = ["B", "KB", "MB", "GB"];
  const i = Math.floor(Math.log(bytes) / Math.log(1024));
  return `${parseFloat((bytes / Math.pow(1024, i)).toFixed(1))}${sizes[i]}`;
};

const formatDuration = (seconds: number): string => {
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${String(s).padStart(2, "0")}`;
};

// Shared styles for lightbox action buttons (download / esc).
const lightboxBtnStyle: React.CSSProperties = {
  color: "var(--c-accent)",
  background: "none",
  border: "1px solid transparent",
  borderRadius: 4,
  cursor: "pointer",
  padding: "2px 8px",
};

const lightboxEscStyle: React.CSSProperties = {
  ...lightboxBtnStyle,
  color: "var(--c-text-dim)",
};

const AttachmentDisplay: React.FC<{ attachment: MessageAttachment }> = ({ attachment }) => {
  const isImage = attachment.content_type.startsWith("image/");
  const isVideo = attachment.content_type.startsWith("video/");
  const isAudio = attachment.content_type.startsWith("audio/");
  // object_key is empty while the upload is still in progress (optimistic stub).
  const isPending = !attachment.object_key;

  // Seed state with the local preview URL if available (images sent by this device).
  const [downloadUrl, setDownloadUrl] = useState<string | null>(
    attachment.localPreviewUrl ?? null
  );
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [viewerOpen, setViewerOpen] = useState(false);
  const [downloadStatus, setDownloadStatus] = useState<"idle" | "downloading" | "done">("idle");
  // Video-specific state.
  const [duration, setDuration] = useState<number | null>(null);
  const [poster, setPoster] = useState<string | null>(null);

  // Revoke the generated poster blob URL when it's replaced or on unmount.
  const prevPosterRef = useRef<string | null>(null);
  useEffect(() => {
    if (prevPosterRef.current && prevPosterRef.current !== poster) {
      URL.revokeObjectURL(prevPosterRef.current);
    }
    prevPosterRef.current = poster;
    return () => {
      if (poster) { URL.revokeObjectURL(poster); }
    };
  }, [poster]);

  // Guard: only attempt poster capture once per component lifetime.
  // Without this, setting downloadUrl (on lightbox open) re-triggers the
  // effect and creates a second GStreamer pipeline on the same blob URL,
  // which races with the lightbox video and causes intermittent failures.
  const posterAttemptedRef = useRef(false);

  // Intercept Escape in the capture phase so AppShell's navigation handler
  // doesn't also fire while the lightbox is open.
  useEffect(() => {
    if (!viewerOpen) { return; }
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopImmediatePropagation();
        setViewerOpen(false);
      }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true });
  }, [viewerOpen]);

  // Video: read duration + capture a poster frame via canvas.
  // Safe to seek now that gst-plugins-good is installed.
  useEffect(() => {
    if (!isVideo || posterAttemptedRef.current) { return; }
    const src = attachment.localPreviewUrl ?? downloadUrl;
    if (!src) { return; }
    // Mark before starting so concurrent dep changes don't trigger a second run.
    posterAttemptedRef.current = true;
    let mounted = true;

    const vid = document.createElement("video");
    vid.muted = true;
    vid.playsInline = true;
    vid.preload = "metadata";

    const cleanup = () => { vid.src = ""; vid.load(); };

    vid.addEventListener("loadedmetadata", () => {
      if (!mounted) { cleanup(); return; }
      if (isFinite(vid.duration) && vid.duration > 0) {
        setDuration(vid.duration);
        // Seek to ~10% of duration for a representative frame.
        vid.currentTime = Math.min(0.5, vid.duration * 0.1);
      } else {
        cleanup();
      }
    }, { once: true });

    vid.addEventListener("seeked", () => {
      if (!mounted) { cleanup(); return; }
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
        ctx.drawImage(vid, 0, 0);
        canvas.toBlob((blob) => {
          if (blob && mounted) { setPoster(URL.createObjectURL(blob)); }
          cleanup();
        }, "image/jpeg", 0.75);
      } else {
        cleanup();
      }
    }, { once: true });

    vid.addEventListener("error", () => { cleanup(); }, { once: true });

    vid.src = src;
    vid.load();

    return () => { mounted = false; cleanup(); };
  }, [isVideo, attachment.localPreviewUrl, downloadUrl]);

  // Auto-load images and audio from R2 once confirmed (object_key populated, no local URL).
  useEffect(() => {
    if ((!isImage && !isAudio) || isPending || downloadUrl) { return; }
    let mounted = true;
    setIsLoading(true);
    downloadAndDecryptMedia(
      attachment.object_key,
      attachment.content_hash,
      attachment.content_type,
    ).then((url) => {
      if (mounted) { setDownloadUrl(url); }
    }).catch((err) => {
      if (mounted) { setError(err instanceof Error ? err.message : "Failed to load"); }
    }).finally(() => {
      if (mounted) { setIsLoading(false); }
    });
    return () => { mounted = false; };
  }, [attachment.object_key]);

  const triggerSave = (url: string) => {
    const a = document.createElement("a");
    a.href = url;
    a.download = attachment.filename;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
  };

  const handleDownload = async () => {
    if (downloadStatus !== "idle") { return; }
    if (downloadUrl) {
      // Already decrypted — save immediately, show brief confirmation.
      triggerSave(downloadUrl);
      setDownloadStatus("done");
      setTimeout(() => setDownloadStatus("idle"), 2000);
      return;
    }
    setDownloadStatus("downloading");
    try {
      const url = await downloadAndDecryptMedia(
        attachment.object_key,
        attachment.content_hash,
        attachment.content_type,
      );
      triggerSave(url);
      setDownloadStatus("done");
      setTimeout(() => setDownloadStatus("idle"), 2000);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to download");
      setDownloadStatus("idle");
    }
  };

  const handleVideoOpen = async () => {
    if (downloadUrl) {
      setViewerOpen(true);
      return;
    }
    setIsLoading(true);
    try {
      const url = await downloadAndDecryptMedia(
        attachment.object_key,
        attachment.content_hash,
        attachment.content_type,
      );
      setDownloadUrl(url);
      setViewerOpen(true);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load");
    } finally {
      setIsLoading(false);
    }
  };

  // ── Shared lightbox action bar ─────────────────────────────────────────────
  const renderLightboxBar = () => (
    <div
      className="flex items-center gap-3 mt-3"
      style={{ cursor: "default" }}
      onClick={(e) => e.stopPropagation()}
    >
      <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
        {attachment.filename}
      </span>
      {attachment.file_size > 0 && (
        <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
          {formatFileSize(attachment.file_size)}
        </span>
      )}
      <button
        onClick={handleDownload}
        disabled={downloadStatus !== "idle"}
        className="text-xs font-mono focus:outline-none focus:ring-2 focus:ring-[var(--c-accent)] focus:ring-offset-1 focus:ring-offset-black flex items-center gap-1"
        style={{ ...lightboxBtnStyle, opacity: downloadStatus !== "idle" ? 1 : undefined }}
        onMouseEnter={(e) => {
          if (downloadStatus !== "idle") { return; }
          (e.currentTarget as HTMLElement).style.background = "var(--c-accent)";
          (e.currentTarget as HTMLElement).style.color = "black";
        }}
        onMouseLeave={(e) => {
          (e.currentTarget as HTMLElement).style.background = "none";
          (e.currentTarget as HTMLElement).style.color = "var(--c-accent)";
        }}
      >
        {downloadStatus === "downloading" ? (
          <>[ fetch <LoadingSpinner size="sm" /> ]</>
        ) : downloadStatus === "done" ? (
          "[ done ]"
        ) : (
          "[download]"
        )}
      </button>
      <button
        onClick={() => setViewerOpen(false)}
        className="text-xs font-mono focus:outline-none focus:ring-2 focus:ring-[var(--c-accent)] focus:ring-offset-1 focus:ring-offset-black"
        style={lightboxEscStyle}
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
  );

  // ── Shared caption bar ─────────────────────────────────────────────────────
  const renderCaptionBar = (extra?: React.ReactNode) => (
    <div
      className="flex items-center gap-2 px-2 py-1"
      style={{ borderTop: "1px solid var(--c-border)" }}
    >
      <span
        className="flex-1 min-w-0 text-xs font-mono truncate"
        style={{ color: "var(--c-accent-dim)" }}
      >
        {attachment.filename}
      </span>
      {extra}
      {attachment.file_size > 0 && (
        <span className="text-xs font-mono flex-shrink-0" style={{ color: "var(--c-text-muted)" }}>
          {formatFileSize(attachment.file_size)}
        </span>
      )}
      {!isPending && (
        <button
          onClick={handleDownload}
          disabled={downloadStatus !== "idle"}
          aria-label={`Download ${attachment.filename}`}
          className="flex-shrink-0 p-1"
          style={{ color: downloadStatus === "done" ? "var(--c-accent)" : "var(--c-text-dim)", lineHeight: 0 }}
        >
          {downloadStatus === "downloading" ? (
            <LoadingSpinner size="sm" />
          ) : downloadStatus === "done" ? (
            <span className="text-xs font-mono">ok</span>
          ) : (
            <Download size={14} aria-hidden="true" />
          )}
        </button>
      )}
    </div>
  );

  // Pre-compute image container height from recorded dimensions (prevents layout shift).
  const imageContainerH = (isImage && attachment.width && attachment.height)
    ? Math.min(Math.round(280 * (attachment.height / attachment.width)), 200)
    : null;

  // ── Image card ─────────────────────────────────────────────────────────────
  if (isImage) {
    return (
      <>
        <div
          data-testid={`attachment-${attachment.id}`}
          style={{
            border: "2px solid var(--c-border)",
            background: "var(--c-surface-high)",
            maxWidth: 280,
            borderRadius: 8,
            overflow: "hidden",
          }}
        >
          {/* Preview area — click to open lightbox */}
          <button
            onClick={() => { if (downloadUrl) { setViewerOpen(true); } }}
            disabled={!downloadUrl}
            aria-label={`View ${attachment.filename}`}
            style={{
              display: "block",
              width: "100%",
              padding: 0,
              background: "none",
              border: 0,
              cursor: downloadUrl ? "zoom-in" : "default",
            }}
          >
            <div
              style={{
                width: "100%",
                height: imageContainerH ?? undefined,
                minHeight: imageContainerH ? undefined : (downloadUrl ? undefined : 80),
                overflow: "hidden",
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
              }}
            >
              {downloadUrl ? (
                <img
                  src={downloadUrl}
                  alt={attachment.filename}
                  // If the blob URL was revoked (e.g. after send), clear it so auto-load kicks in.
                  onError={() => setDownloadUrl(null)}
                  style={{
                    width: "100%",
                    height: imageContainerH ? "100%" : undefined,
                    maxHeight: imageContainerH ? undefined : 200,
                    objectFit: "contain",
                    display: "block",
                  }}
                />
              ) : attachment.blurhash && attachment.width && attachment.height ? (
                <BlurhashCanvas
                  hash={attachment.blurhash}
                  width={attachment.width}
                  height={attachment.height}
                />
              ) : (
                <div style={{ height: imageContainerH ?? 80, width: "100%", display: "flex", alignItems: "center", justifyContent: "center" }}>
                  <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                    {error ? "err" : "…"}
                  </span>
                </div>
              )}
            </div>
          </button>

          {renderCaptionBar()}
        </div>

        {/* Full-screen lightbox */}
        {viewerOpen && downloadUrl && (
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
            onClick={() => setViewerOpen(false)}
          >
            <img
              src={downloadUrl}
              alt={attachment.filename}
              style={{
                maxWidth: "90vw",
                maxHeight: "85vh",
                objectFit: "contain",
                cursor: "default",
                borderRadius: "1rem",
              }}
              onClick={(e) => e.stopPropagation()}
            />
            <div onClick={(e) => e.stopPropagation()}>
              {renderLightboxBar()}
            </div>
          </div>
        )}
      </>
    );
  }

  // ── Audio card ─────────────────────────────────────────────────────────────
  if (isAudio) {
    return (
      <div
        data-testid={`attachment-${attachment.id}`}
        style={{
          border: "2px solid var(--c-border)",
          background: "var(--c-surface-high)",
          borderRadius: 8,
          overflow: "hidden",
          width: "100%",
          maxWidth: 600,
        }}
      >
        <div className="flex items-center gap-2 px-3 py-2">
          <Music size={16} aria-hidden="true" style={{ color: "var(--c-text-dim)", flexShrink: 0 }} />
          {downloadUrl ? (
            <audio
              controls
              src={downloadUrl}
              style={{ flex: 1, minWidth: 0, height: 32 }}
            />
          ) : (
            <div className="flex-1 flex items-center">
              <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                {isLoading ? "loading…" : error ? error : "…"}
              </span>
            </div>
          )}
        </div>
        {renderCaptionBar()}
      </div>
    );
  }

  // ── Video card ─────────────────────────────────────────────────────────────
  if (isVideo) {
    return (
      <>
        <div
          data-testid={`attachment-${attachment.id}`}
          style={{
            border: "2px solid var(--c-border)",
            background: "var(--c-surface-high)",
            width: 200,
            borderRadius: 8,
            overflow: "hidden",
          }}
        >
          {/* Preview area — click to open lightbox */}
          <button
            onClick={!isPending && !isLoading ? handleVideoOpen : undefined}
            disabled={isPending || isLoading}
            aria-label={`Open ${attachment.filename}`}
            style={{
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              width: "100%",
              height: 112,
              padding: 0,
              background: "var(--c-bg)",
              border: 0,
              cursor: isPending || isLoading ? "default" : "pointer",
              position: "relative",
              overflow: "hidden",
            }}
          >
            {/* Poster: generated frame > blurhash > film icon */}
            {(poster || (attachment.blurhash && attachment.width && attachment.height)) && (
              <div style={{ position: "absolute", inset: 0, overflow: "hidden" }}>
                {poster ? (
                  <img
                    src={poster}
                    alt=""
                    aria-hidden="true"
                    style={{ width: "100%", height: "100%", objectFit: "cover" }}
                  />
                ) : (
                  <BlurhashCanvas
                    hash={attachment.blurhash!}
                    width={attachment.width!}
                    height={attachment.height!}
                  />
                )}
              </div>
            )}
            {/* Play icon overlay — always shown so users know it's clickable */}
            <div style={{
              position: "relative",
              zIndex: 1,
              width: 36,
              height: 36,
              borderRadius: "50%",
              background: "rgba(0,0,0,0.55)",
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
            }}>
              {isLoading ? (
                <LoadingSpinner size="sm" />
              ) : (
                <Film size={18} aria-hidden="true" style={{ color: "white" }} />
              )}
            </div>
          </button>

          {renderCaptionBar(
            duration != null ? (
              <span className="text-xs font-mono flex-shrink-0" style={{ color: "var(--c-text-muted)" }}>
                {formatDuration(duration)}
              </span>
            ) : undefined
          )}
        </div>

        {/* Video lightbox */}
        {viewerOpen && downloadUrl && (
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
            onClick={() => setViewerOpen(false)}
          >
            {/* autoFocus lets the browser handle Space/Enter for play/pause */}
            <video
              autoFocus
              src={downloadUrl}
              controls
              style={{
                maxWidth: "90vw",
                maxHeight: "85vh",
                cursor: "default",
                borderRadius: "1rem",
              }}
              onClick={(e) => e.stopPropagation()}
            />
            <div onClick={(e) => e.stopPropagation()}>
              {renderLightboxBar()}
            </div>
          </div>
        )}
      </>
    );
  }

  // ── Generic file card ──────────────────────────────────────────────────────
  return (
    <div
      data-testid={`attachment-${attachment.id}`}
      className="flex items-center gap-2 px-2.5 py-1.5"
      style={{
        border: "2px solid var(--c-border)",
        background: "var(--c-surface-high)",
        minWidth: 160,
        maxWidth: 240,
        borderRadius: 8,
      }}
    >
      <FileIcon size={16} aria-hidden="true" style={{ color: "var(--c-text-dim)", flexShrink: 0 }} />
      <div className="flex-1 min-w-0">
        <div className="text-xs font-mono truncate" style={{ color: "var(--c-accent-dim)" }}>
          {attachment.filename}
        </div>
        {attachment.file_size > 0 && (
          <div className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
            {formatFileSize(attachment.file_size)}
          </div>
        )}
      </div>
      {error ? (
        <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>err</span>
      ) : isPending ? (
        <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>…</span>
      ) : (
        <button
          onClick={handleDownload}
          disabled={isLoading}
          aria-label={`Download ${attachment.filename}`}
          className="p-1"
          style={{ color: "var(--c-text-dim)", flexShrink: 0, lineHeight: 0 }}
        >
          {isLoading ? "…" : <Download size={14} aria-hidden="true" />}
        </button>
      )}
    </div>
  );
};
