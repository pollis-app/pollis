import { errorMessage } from "../../utils/errorMessage";
import React, { useState, useEffect, useRef } from "react";
import { decode } from "blurhash";
import { dialogSave, writeFile } from "../../bridge";
import { Download, Film, Check } from "lucide-react";
import { getFileIcon } from "../../utils/fileIcon";
import { captureVideoPoster } from "../../utils/imageProcessing";
import { formatFileSize, formatDuration } from "../../utils/format";
import { downloadAndDecryptMedia, getMediaUrl } from "../../services/r2-upload";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import { InlineAudioPlayer } from "../ui/InlineAudioPlayer";
import { AudioPlayer } from "../ui/AudioPlayer";
import type { MessageAttachment } from "../../types";

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

// Shared styles for lightbox action buttons (download / esc).
const lightboxBtnStyle: React.CSSProperties = {
  border: "2px solid transparent",
  borderRadius: 4,
  cursor: "pointer",
  padding: "2px 8px",
};

export const AttachmentDisplay: React.FC<{ attachment: MessageAttachment }> = ({ attachment }) => {
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
  // Tracks the specific src we last attempted a poster capture for. A boolean
  // latch here would block a late-arriving downloadUrl from ever producing a
  // poster; keying on the src value lets a new src re-run exactly once.
  const posterAttemptedSrcRef = useRef<string | null>(null);

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

  // After the lightbox closes, return focus to the chat input. MessageItem
  // doesn't own the input ref, so signal via a window event that MainContent
  // listens for. Only fire on a true→false transition so unrelated unmounts
  // (virtualized scroll, channel switch) don't steal focus.
  const prevViewerOpenRef = useRef(viewerOpen);
  useEffect(() => {
    if (prevViewerOpenRef.current && !viewerOpen) {
      window.dispatchEvent(new Event("pollis:focus-chat-input"));
    }
    prevViewerOpenRef.current = viewerOpen;
  }, [viewerOpen]);

  // Video: read duration + capture a poster frame via canvas.
  // Safe to seek now that gst-plugins-good is installed.
  useEffect(() => {
    if (!isVideo) { return; }
    const src = attachment.localPreviewUrl ?? downloadUrl;
    if (!src || posterAttemptedSrcRef.current === src) { return; }
    // Mark this src before starting so concurrent dep changes don't trigger a
    // second run for the same src, while a *new* src can still re-run.
    posterAttemptedSrcRef.current = src;
    let mounted = true;

    captureVideoPoster(src).then((result) => {
      if (!result) { return; }
      // Component unmounted mid-capture — drop the poster and revoke its blob
      // URL so it doesn't leak (the poster-revoke effect only tracks state).
      if (!mounted) { URL.revokeObjectURL(result.url); return; }
      if (result.duration > 0) { setDuration(result.duration); }
      setPoster(result.url);
    });

    return () => { mounted = false; };
  }, [isVideo, attachment.localPreviewUrl, downloadUrl]);

  // Guard against a fetch-succeeds-but-render-fails loop: `<img onError>` nulls
  // downloadUrl, which re-fires the auto-load effect below. A URL that fetches
  // fine yet fails to render would otherwise loop forever (fetch→render→error→
  // null→fetch). Cap how many render failures we retry before giving up.
  const renderFailuresRef = useRef(0);
  const MAX_RENDER_RETRIES = 1;

  // Auto-load images and audio from R2 once confirmed (object_key populated, no local URL).
  // One URL pattern across image and audio: the loopback media server
  // returns `http://127.0.0.1:<port>/<token>/<hash>`. Bytes never cross
  // the JSON IPC; disk cache is encrypted at rest under the session
  // db_key; HTTP Range works for `<audio>` / `<video>` natively.
  useEffect(() => {
    if ((!isImage && !isAudio) || isPending || downloadUrl) {
      return;
    }
    // A previously fetched URL rendered and then errored past the retry cap —
    // stop re-fetching, surface the failure instead of spinning.
    if (renderFailuresRef.current > MAX_RENDER_RETRIES) {
      setError("Failed to load");
      return;
    }

    let mounted = true;
    setIsLoading(true);
    const fetchUrl = getMediaUrl(
      attachment.object_key,
      attachment.content_hash,
      attachment.content_type,
    );
    fetchUrl.then((url) => {
      if (mounted) { setDownloadUrl(url); }
    }).catch((err) => {
      if (mounted) { setError(errorMessage(err, "Failed to load")); }
    }).finally(() => {
      if (mounted) { setIsLoading(false); }
    });
    return () => { mounted = false; };
    // Key on every input the guard reads, not just object_key. Previously a
    // confirmed attachment (isPending flips false) or a downloadUrl reset back
    // to null (failed load) never re-fired this effect, so the media never retried.
  }, [isImage, isAudio, isPending, downloadUrl, attachment.object_key, attachment.content_hash, attachment.content_type]);

  // Revoke decrypted blob URLs we created when they're replaced or on unmount.
  // Skip non-blob URLs (e.g. tauri convertFileSrc paths) and skip the
  // sender-owned localPreviewUrl, which is freed by the optimistic-send code.
  const ownedBlobRef = useRef<string | null>(null);
  useEffect(() => {
    const isBlob = !!downloadUrl && downloadUrl.startsWith("blob:");
    const isOwnedByUs = isBlob && downloadUrl !== attachment.localPreviewUrl;
    const prev = ownedBlobRef.current;
    if (prev && prev !== downloadUrl) {
      URL.revokeObjectURL(prev);
    }
    ownedBlobRef.current = isOwnedByUs ? downloadUrl : null;
    return () => {
      if (ownedBlobRef.current) {
        URL.revokeObjectURL(ownedBlobRef.current);
        ownedBlobRef.current = null;
      }
    };
  }, [downloadUrl, attachment.localPreviewUrl]);

  // Save to a user-chosen path via the native Tauri dialog. The `<a download>`
  // trick doesn't work on WebKitGTK: `download` is ignored across origins
  // (loopback media URL vs the app's tauri:// origin), so the webview just
  // navigates to the URL and shows the browser's built-in audio/video player
  // instead of triggering a download.
  const triggerSave = async (url: string): Promise<boolean> => {
    const target = await dialogSave({ defaultPath: attachment.filename });
    if (!target) {
      return false;
    }
    const res = await fetch(url);
    if (!res.ok) {
      throw new Error(`fetch failed: ${res.status}`);
    }
    const bytes = new Uint8Array(await res.arrayBuffer());
    await writeFile(target, bytes);
    return true;
  };

  const handleDownload = async () => {
    if (downloadStatus !== "idle") { return; }
    setDownloadStatus("downloading");
    try {
      const url = downloadUrl
        ?? await downloadAndDecryptMedia(
          attachment.object_key,
          attachment.content_hash,
          attachment.content_type,
        );
      const saved = await triggerSave(url);
      setDownloadStatus(saved ? "done" : "idle");
      if (saved) {
        setTimeout(() => setDownloadStatus("idle"), 2000);
      }
    } catch (err) {
      setError(errorMessage(err, "Failed to download"));
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
      // Video uses the same loopback URL pattern as image/audio. The
      // server honours HTTP Range so seeking works without buffering
      // the whole file.
      const url = await getMediaUrl(
        attachment.object_key,
        attachment.content_hash,
        attachment.content_type,
      );
      setDownloadUrl(url);
      setViewerOpen(true);
    } catch (err) {
      setError(errorMessage(err, "Failed to load"));
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
        className="text-xs font-mono transition-colors text-[var(--c-accent)] bg-transparent enabled:hover:bg-[var(--c-accent)] enabled:hover:text-black focus:outline-none focus:ring-2 focus:ring-[var(--c-accent)] focus:ring-offset-1 focus:ring-offset-black flex items-center gap-1"
        style={{ ...lightboxBtnStyle, opacity: downloadStatus !== "idle" ? 1 : undefined }}
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
        className="text-xs font-mono transition-colors text-[var(--c-text-dim)] bg-transparent hover:bg-[var(--c-accent)] hover:text-black focus:outline-none focus:ring-2 focus:ring-[var(--c-accent)] focus:ring-offset-1 focus:ring-offset-black"
        style={lightboxBtnStyle}
      >
        [esc]
      </button>
    </div>
  );

  // ── Shared caption bar ─────────────────────────────────────────────────────
  const renderCaptionBar = (extra?: React.ReactNode) => (
    <div
      className="flex items-center gap-2 px-2 py-1"
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
            <Check size={14} aria-hidden="true" />
          ) : (
            <Download size={14} aria-hidden="true" />
          )}
        </button>
      )}
    </div>
  );

  // ── Image card — uniform 96×96 thumb, click to open lightbox ──────────────
  if (isImage) {
    return (
      <>
        <button
          data-testid={`attachment-${attachment.id}`}
          onClick={() => { if (downloadUrl) { setViewerOpen(true); } }}
          disabled={!downloadUrl}
          aria-label={`View ${attachment.filename}`}
          title={attachment.filename}
          style={{
            width: 96,
            height: 96,
            padding: 0,
            background: "transparent",
            border: "none",
            borderRadius: "0.5rem",
            overflow: "hidden",
            cursor: downloadUrl ? "zoom-in" : "default",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            flexShrink: 0,
          }}
        >
          {downloadUrl ? (
            <img
              src={downloadUrl}
              alt={attachment.filename}
              onError={() => {
                renderFailuresRef.current += 1;
                setDownloadUrl(null);
              }}
              style={{
                width: "100%",
                height: "100%",
                objectFit: "cover",
                display: "block",
              }}
            />
          ) : attachment.blurhash && attachment.width && attachment.height ? (
            <div style={{ width: "100%", height: "100%", overflow: "hidden" }}>
              <BlurhashCanvas
                hash={attachment.blurhash}
                width={attachment.width}
                height={attachment.height}
              />
            </div>
          ) : (
            <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
              {error ? "err" : "…"}
            </span>
          )}
        </button>

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
      <>
        <div
          data-testid={`attachment-${attachment.id}`}
          style={{
            borderRadius: 8,
            overflow: "hidden",
            width: "100%",
            maxWidth: 600,
          }}
        >
          {downloadUrl ? (
            <InlineAudioPlayer
              src={downloadUrl}
              title={attachment.filename}
              onClick={() => setViewerOpen(true)}
            />
          ) : (
            <div
              className="flex items-center gap-2 px-3 py-2"
              style={{
                background: "var(--c-surface-high)",
                borderRadius: 8,
              }}
            >
              <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                {isLoading ? "loading…" : error ? error : "…"}
              </span>
            </div>
          )}
          {renderCaptionBar()}
        </div>

        {/* Audio lightbox — full player */}
        {viewerOpen && downloadUrl && (
          <div
            style={{
              position: "fixed",
              inset: 0,
              zIndex: 9999,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              flexDirection: "column",
              background: "rgba(0,0,0,0.85)",
              cursor: "pointer",
            }}
            onClick={() => setViewerOpen(false)}
          >
            <div
              style={{ width: "90vw", maxWidth: 500 }}
              onClick={(e) => e.stopPropagation()}
            >
              <AudioPlayer
                src={downloadUrl}
                title={attachment.filename}
                autoPlay
              />
            </div>
            <div onClick={(e) => e.stopPropagation()}>
              {renderLightboxBar()}
            </div>
          </div>
        )}
      </>
    );
  }

  // ── Video card — uniform 96×96 thumb with play overlay ────────────────────
  if (isVideo) {
    return (
      <>
        <button
          data-testid={`attachment-${attachment.id}`}
          onClick={!isPending && !isLoading ? handleVideoOpen : undefined}
          disabled={isPending || isLoading}
          aria-label={`Open ${attachment.filename}`}
          title={attachment.filename}
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            width: 96,
            height: 96,
            padding: 0,
            background: "transparent",
            border: "none",
            cursor: isPending || isLoading ? "default" : "pointer",
            position: "relative",
            overflow: "hidden",
            borderRadius: "0.5rem",
            flexShrink: 0,
          }}
        >
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
          <div style={{
            position: "relative",
            zIndex: 1,
            width: 28,
            height: 28,
            borderRadius: "50%",
            background: "rgba(0,0,0,0.55)",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
          }}>
            {isLoading ? (
              <LoadingSpinner size="sm" />
            ) : (
              <Film size={14} aria-hidden="true" style={{ color: "white" }} />
            )}
          </div>
          {duration != null && (
            <span
              className="font-mono"
              style={{
                position: "absolute",
                bottom: 4,
                right: 4,
                zIndex: 2,
                fontSize: 10,
                lineHeight: 1,
                padding: "2px 4px",
                borderRadius: 2,
                background: "rgba(0,0,0,0.65)",
                color: "white",
              }}
            >
              {formatDuration(duration)}
            </span>
          )}
        </button>

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
  const FileTypeIcon = getFileIcon(attachment.filename);
  return (
    <div
      data-testid={`attachment-${attachment.id}`}
      className="flex items-center gap-2 px-2.5 py-1.5 min-w-0"
      style={{
        border: "2px solid var(--c-border)",
        background: "var(--c-surface-high)",
        maxWidth: 360,
        borderRadius: 8,
      }}
    >
      <FileTypeIcon size={14} aria-hidden="true" style={{ color: "var(--c-text-dim)", flexShrink: 0 }} />
      {(() => {
        const lastDot = attachment.filename.lastIndexOf(".");
        const hasExt = lastDot > 0 && lastDot < attachment.filename.length - 1;
        const head = hasExt ? attachment.filename.slice(0, lastDot) : attachment.filename;
        const tail = hasExt ? attachment.filename.slice(lastDot) : "";
        return (
          <span
            className="text-sm font-mono flex-1 min-w-0 flex"
            title={attachment.filename}
            style={{ color: "var(--c-accent-dim)" }}
          >
            <span className="truncate">{head}</span>
            {tail && <span className="flex-shrink-0">{tail}</span>}
          </span>
        );
      })()}
      {attachment.file_size > 0 && (
        <span className="text-sm font-mono flex-shrink-0" style={{ color: "var(--c-text-muted)" }}>
          {formatFileSize(attachment.file_size)}
        </span>
      )}
      {error ? (
        <span className="text-sm font-mono flex-shrink-0" style={{ color: "var(--c-text-muted)" }}>err</span>
      ) : isPending ? (
        <span className="text-sm font-mono flex-shrink-0" style={{ color: "var(--c-text-muted)" }}>…</span>
      ) : (
        <button
          onClick={handleDownload}
          disabled={downloadStatus !== "idle"}
          aria-label={`Download ${attachment.filename}`}
          className="flex-shrink-0"
          style={{
            color: downloadStatus === "done" ? "var(--c-accent)" : "var(--c-text-dim)",
            lineHeight: 0,
            background: "none",
            border: "none",
            padding: 0,
          }}
        >
          {downloadStatus === "downloading" ? (
            <LoadingSpinner size="sm" />
          ) : downloadStatus === "done" ? (
            <Check size={14} aria-hidden="true" />
          ) : (
            <Download size={14} aria-hidden="true" />
          )}
        </button>
      )}
    </div>
  );
};
