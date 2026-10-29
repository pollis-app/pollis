import { errorMessage } from "../../utils/errorMessage";
import React, { useState, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { dialogSave, invoke } from "../../bridge";
import { Download, Film, Check } from "lucide-react";
import { getFileIcon } from "../../utils/fileIcon";
import { captureVideoPoster } from "../../utils/imageProcessing";
import { formatFileSize, formatDuration } from "../../utils/format";
import { getMediaUrl, mediaArgs } from "../../services/r2-upload";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import { InlineAudioPlayer } from "../ui/InlineAudioPlayer";
import { AudioPlayer } from "../ui/AudioPlayer";
import type { MessageAttachment } from "../../types";
import { PressButton } from "../ui/PressButton";
import { BlurhashCanvas } from "../Media/BlurhashCanvas";
import { SensitiveCover } from "../Media/SensitiveCover";
import { SensitiveHideButton } from "../Media/SensitiveHideButton";
import { useSensitiveReveal } from "../../hooks/useSensitiveReveal";

const lightboxBtnStyle: React.CSSProperties = {
  border: "2px solid transparent",
  borderRadius: 4,
  cursor: "pointer",
  padding: "2px 8px",
};

interface AttachmentDisplayProps {
  attachment: MessageAttachment;
  /** Grid mode: the card fills its cell as a square instead of the 96×96 thumb. */
  tile?: boolean;
  /** Overrides the default `attachment-<id>` test id. */
  testId?: string;
}

export const AttachmentDisplay = observer(({
  attachment,
  tile = false,
  testId,
}: AttachmentDisplayProps) => {
  const { t } = useTranslation("chat");
  // Sensitive media (#1568): the reveal is shared across surfaces.
  const sensitivity = useSensitiveReveal(attachment);
  const isImage = attachment.content_type.startsWith("image/");
  const isVideo = attachment.content_type.startsWith("video/");
  const isAudio = attachment.content_type.startsWith("audio/");
  const cardTestId = testId ?? `attachment-${attachment.id}`;
  const thumbBox: React.CSSProperties = tile
    ? { width: "100%", aspectRatio: "1 / 1" }
    : { width: 96, height: 96 };
  const isPending = !attachment.object_key;

  // Own box so the hide button can be a sibling: a button cannot hold another.
  const withSensitiveChrome = (thumb: React.ReactNode) =>
    sensitivity.sensitive ? (
      <div className="relative flex-shrink-0" style={thumbBox}>
        {thumb}
        {!sensitivity.hidden && (
          <SensitiveHideButton filename={attachment.filename} onHide={sensitivity.hide} />
        )}
      </div>
    ) : (
      thumb
    );
  // A click only reveals, so a video's reveal never starts playback.
  const hiddenThumb = () => (
    <PressButton
      data-testid={cardTestId}
      data-sensitive="hidden"
      activateOn="click"
      onClick={sensitivity.reveal}
      aria-label={t("attachment.sensitiveReveal", { filename: attachment.filename })}
      title={attachment.filename}
      className="border-0"
      style={{
        width: "100%",
        height: "100%",
        padding: 0,
        background: "transparent",
        borderRadius: "0.5rem",
        overflow: "hidden",
        position: "relative",
        cursor: "pointer",
        display: "block",
      }}
    >
      <SensitiveCover blurhash={attachment.blurhash} />
    </PressButton>
  );
  const cardBox: React.CSSProperties = sensitivity.sensitive
    ? { width: "100%", height: "100%" }
    : thumbBox;

  const [downloadUrl, setDownloadUrl] = useState<string | null>(
    attachment.localPreviewUrl ?? null
  );
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [viewerOpen, setViewerOpen] = useState(false);
  const [downloadStatus, setDownloadStatus] = useState<"idle" | "downloading" | "done">("idle");
  const [duration, setDuration] = useState<number | null>(null);
  const [poster, setPoster] = useState<string | null>(null);

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

  // Last src a poster capture ran for: a second GStreamer pipeline on the same blob races the lightbox.
  const posterAttemptedSrcRef = useRef<string | null>(null);

  // Capture phase, so AppShell's Escape handler doesn't also fire.
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

  // Return focus to the chat input on close only (true→false), via a window event.
  const prevViewerOpenRef = useRef(viewerOpen);
  useEffect(() => {
    if (prevViewerOpenRef.current && !viewerOpen) {
      window.dispatchEvent(new Event("pollis:focus-chat-input"));
    }
    prevViewerOpenRef.current = viewerOpen;
  }, [viewerOpen]);

  useEffect(() => {
    if (!isVideo) { return; }
    const src = attachment.localPreviewUrl ?? downloadUrl;
    if (!src || posterAttemptedSrcRef.current === src) { return; }
    posterAttemptedSrcRef.current = src;
    let mounted = true;

    captureVideoPoster(src).then((result) => {
      if (!result) { return; }
      if (!mounted) { URL.revokeObjectURL(result.url); return; }
      if (result.duration > 0) { setDuration(result.duration); }
      setPoster(result.url);
    });

    return () => { mounted = false; };
  }, [isVideo, attachment.localPreviewUrl, downloadUrl]);

  // Caps render-failure retries: onError nulls downloadUrl, which re-fetches.
  const renderFailuresRef = useRef(0);
  const MAX_RENDER_RETRIES = 1;

  // Auto-load images and audio once confirmed, via the loopback media server.
  useEffect(() => {
    if ((!isImage && !isAudio) || isPending || downloadUrl) {
      return;
    }
    if (renderFailuresRef.current > MAX_RENDER_RETRIES) {
      setError(t("attachment.failedToLoad"));
      return;
    }

    let mounted = true;
    setIsLoading(true);
    const fetchUrl = getMediaUrl(attachment);
    fetchUrl.then((url) => {
      if (mounted) { setDownloadUrl(url); }
    }).catch((err) => {
      if (mounted) { setError(errorMessage(err, t("attachment.failedToLoad"))); }
    }).finally(() => {
      if (mounted) { setIsLoading(false); }
    });
    return () => { mounted = false; };
  }, [isImage, isAudio, isPending, downloadUrl, attachment.object_key, attachment.content_hash, attachment.content_type]);

  // Revoke blobs we created; localPreviewUrl is freed by the optimistic send.
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

  // Rust writes the file (`save_media_to_path`) so it gets the platform quarantine marker; see pollis-core/src/downloads.rs.
  const triggerSave = async (): Promise<boolean> => {
    const target = await dialogSave({ defaultPath: attachment.filename });
    if (!target) {
      return false;
    }
    const marking = await invoke<string>("save_media_to_path", {
      ...mediaArgs(attachment),
      targetPath: target,
    });
    // A failed marker (FAT32, network share) doesn't fail the save, but is reported.
    if (marking.startsWith("failed")) {
      console.warn("[attachment] saved without a download marker:", marking);
    }
    return true;
  };

  const handleDownload = async () => {
    if (downloadStatus !== "idle") { return; }
    setDownloadStatus("downloading");
    try {
      const saved = await triggerSave();
      setDownloadStatus(saved ? "done" : "idle");
      if (saved) {
        setTimeout(() => setDownloadStatus("idle"), 2000);
      }
    } catch (err) {
      setError(errorMessage(err, t("attachment.failedToDownload")));
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
      const url = await getMediaUrl(attachment);
      setDownloadUrl(url);
      setViewerOpen(true);
    } catch (err) {
      setError(errorMessage(err, t("attachment.failedToLoad")));
    } finally {
      setIsLoading(false);
    }
  };

  const renderLightboxBar = () => (
    <div
      className="flex items-center gap-3 mt-3"
      style={{ cursor: "default" }}
      onClick={(e) => e.stopPropagation()}
    >
      <span className="text-xs font-mono text-muted">
        {attachment.filename}
      </span>
      {attachment.file_size > 0 && (
        <span className="text-xs font-mono text-muted">
          {formatFileSize(attachment.file_size)}
        </span>
      )}
      <button
        onClick={handleDownload}
        disabled={downloadStatus !== "idle"}
        className="text-xs font-mono transition-colors text-accent bg-transparent enabled:hover:bg-accent enabled:hover:text-black focus:outline-none focus:ring-2 focus:ring-accent focus:ring-offset-1 focus:ring-offset-black flex items-center gap-1"
        style={{ ...lightboxBtnStyle, opacity: downloadStatus !== "idle" ? 1 : undefined }}
      >
        {downloadStatus === "downloading" ? (
          <>[ {t("attachment.lightboxFetch")} <LoadingSpinner size="sm" /> ]</>
        ) : downloadStatus === "done" ? (
          t("attachment.lightboxDone")
        ) : (
          t("attachment.lightboxDownload")
        )}
      </button>
      <button
        onClick={() => setViewerOpen(false)}
        className="text-xs font-mono transition-colors text-dim bg-transparent hover:bg-accent hover:text-black focus:outline-none focus:ring-2 focus:ring-accent focus:ring-offset-1 focus:ring-offset-black"
        style={lightboxBtnStyle}
      >
        {t("attachment.lightboxClose")}
      </button>
    </div>
  );

  const renderCaptionBar = (extra?: React.ReactNode) => (
    <div
      className="flex items-center gap-2 px-2 py-1"
    >
      <span
        className="flex-1 min-w-0 text-xs font-mono truncate text-accent-dim"
      >
        {attachment.filename}
      </span>
      {extra}
      {attachment.file_size > 0 && (
        <span className="text-xs font-mono flex-shrink-0 text-muted">
          {formatFileSize(attachment.file_size)}
        </span>
      )}
      {!isPending && (
        <button
          onClick={handleDownload}
          disabled={downloadStatus !== "idle"}
          aria-label={t("attachment.downloadLabel", { filename: attachment.filename })}
          className={`flex-shrink-0 p-1 ${downloadStatus === "done" ? "text-accent" : "text-dim"}`}
          style={{ lineHeight: 0 }}
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

  if (isImage) {
    return (
      <>
        {withSensitiveChrome(sensitivity.hidden ? hiddenThumb() : (
        <PressButton
          data-testid={cardTestId}
          data-sensitive={sensitivity.sensitive ? "revealed" : undefined}
          onClick={() => { if (downloadUrl) { setViewerOpen(true); } }}
          disabled={!downloadUrl}
          aria-label={t("attachment.viewLabel", { filename: attachment.filename })}
          title={attachment.filename}
          className="border-0"
          style={{
            ...cardBox,
            padding: 0,
            background: "transparent",
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
          ) : attachment.blurhash ? (
            <div style={{ width: "100%", height: "100%", overflow: "hidden" }}>
              <BlurhashCanvas hash={attachment.blurhash} />
            </div>
          ) : (
            <span className="text-xs font-mono text-muted">
              {error ? t("attachment.error") : "…"}
            </span>
          )}
        </PressButton>
        ))}

        {viewerOpen && downloadUrl && !sensitivity.hidden && (
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

  if (isAudio) {
    return (
      <>
        <div
          data-testid={cardTestId}
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
              activateOn="press"
              onClick={() => setViewerOpen(true)}
            />
          ) : (
            <div
              className="flex items-center gap-2 px-3 py-2 bg-surface-high"
              style={{
                borderRadius: 8,
              }}
            >
              <span className="text-xs font-mono text-muted">
                {isLoading ? t("attachment.loading") : error ? error : "…"}
              </span>
            </div>
          )}
          {renderCaptionBar()}
        </div>

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

  if (isVideo) {
    return (
      <>
        {withSensitiveChrome(sensitivity.hidden ? hiddenThumb() : (
        <PressButton
          data-testid={cardTestId}
          data-sensitive={sensitivity.sensitive ? "revealed" : undefined}
          onClick={!isPending && !isLoading ? handleVideoOpen : undefined}
          disabled={isPending || isLoading}
          aria-label={t("attachment.openLabel", { filename: attachment.filename })}
          title={attachment.filename}
          className="border-0"
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            ...cardBox,
            padding: 0,
            background: "transparent",
            cursor: isPending || isLoading ? "default" : "pointer",
            position: "relative",
            overflow: "hidden",
            borderRadius: "0.5rem",
            flexShrink: 0,
          }}
        >
          {(poster || attachment.blurhash) && (
            <div style={{ position: "absolute", inset: 0, overflow: "hidden" }}>
              {poster ? (
                <img
                  src={poster}
                  alt=""
                  aria-hidden="true"
                  style={{ width: "100%", height: "100%", objectFit: "cover" }}
                />
              ) : (
                <BlurhashCanvas hash={attachment.blurhash!} />
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
                insetInlineEnd: 4,
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
        </PressButton>
        ))}

        {viewerOpen && downloadUrl && !sensitivity.hidden && (
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

  const FileTypeIcon = getFileIcon(attachment.filename);
  return (
    <div
      data-testid={cardTestId}
      className="flex items-center gap-2 px-2.5 py-1.5 min-w-0 border-2 border-line bg-surface-high"
      style={{
        maxWidth: 360,
        borderRadius: 8,
      }}
    >
      <FileTypeIcon size={14} aria-hidden="true" className="text-dim" style={{ flexShrink: 0 }} />
      {(() => {
        const lastDot = attachment.filename.lastIndexOf(".");
        const hasExt = lastDot > 0 && lastDot < attachment.filename.length - 1;
        const head = hasExt ? attachment.filename.slice(0, lastDot) : attachment.filename;
        const tail = hasExt ? attachment.filename.slice(lastDot) : "";
        return (
          <span
            className="text-sm font-mono flex-1 min-w-0 flex text-accent-dim"
            title={attachment.filename}
          >
            <span className="truncate">{head}</span>
            {tail && <span className="flex-shrink-0">{tail}</span>}
          </span>
        );
      })()}
      {attachment.file_size > 0 && (
        <span className="text-sm font-mono flex-shrink-0 text-muted">
          {formatFileSize(attachment.file_size)}
        </span>
      )}
      {error ? (
        <span className="text-sm font-mono flex-shrink-0 text-muted">{t("attachment.error")}</span>
      ) : isPending ? (
        <span className="text-sm font-mono flex-shrink-0 text-muted">…</span>
      ) : (
        <button
          onClick={handleDownload}
          disabled={downloadStatus !== "idle"}
          aria-label={t("attachment.downloadLabel", { filename: attachment.filename })}
          className={`flex-shrink-0 border-0 ${downloadStatus === "done" ? "text-accent" : "text-dim"}`}
          style={{
            lineHeight: 0,
            background: "none",
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
});
