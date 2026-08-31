import React, { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronLeft, ChevronRight, FileIcon, Play, X } from "lucide-react";
import { getMediaUrl } from "../../services/r2-upload";
import { AttachmentDisplay } from "../Message/AttachmentDisplay";
import { useSkin } from "../../hooks/queries/usePreferences";
import type { MessageAttachment } from "../../types";

interface MediaGalleryViewProps {
  /** Every attachment in the surface, newest first — the caller orders. */
  attachments: MessageAttachment[];
  /** Rendered when there is nothing to show. */
  emptyLabel: string;
}

/**
 * A Google-Photos-style roll over a set of attachments (#107): images and
 * videos as a responsive tile grid with a keyboard-navigable viewer, other
 * files as a flat list below.
 *
 * Deliberately GENERIC — it takes a bare `MessageAttachment[]` and knows
 * nothing about vaults, channels or DMs — so the same component can later
 * back a media view for any conversation. Today the Vault is its only
 * caller.
 *
 * Bytes resolve through the loopback media server via `getMediaUrl`, sharing
 * its in-flight cache with every other attachment surface. The full-screen
 * viewer follows the exact overlay shape `AttachmentDisplay`'s lightbox
 * established (the codebase's one sanctioned full-bleed surface), plus
 * prev/next so the roll can be walked without closing it.
 */
export const MediaGalleryView: React.FC<MediaGalleryViewProps> = ({
  attachments,
  emptyLabel,
}) => {
  const { t } = useTranslation("vault");
  const isTerminal = useSkin() !== "refined";
  // Viewer state: index into `media`, or null when closed.
  const [viewerIndex, setViewerIndex] = useState<number | null>(null);

  const media = useMemo(
    () =>
      attachments.filter(
        (a) =>
          a.content_type.startsWith("image/") ||
          a.content_type.startsWith("video/"),
      ),
    [attachments],
  );
  const files = useMemo(
    () =>
      attachments.filter(
        (a) =>
          !a.content_type.startsWith("image/") &&
          !a.content_type.startsWith("video/"),
      ),
    [attachments],
  );

  const step = useCallback(
    (delta: number) => {
      setViewerIndex((current) => {
        if (current === null || media.length === 0) {
          return current;
        }
        return (current + delta + media.length) % media.length;
      });
    },
    [media.length],
  );

  // Viewer keyboard model: arrows walk the roll, Escape closes. Capture
  // phase + stopImmediatePropagation for Escape, because the window-level
  // nav.back shortcut also binds it and would navigate the page away.
  useEffect(() => {
    if (viewerIndex === null) {
      return;
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopImmediatePropagation();
        setViewerIndex(null);
      } else if (event.key === "ArrowRight") {
        step(1);
      } else if (event.key === "ArrowLeft") {
        step(-1);
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [viewerIndex, step]);

  if (attachments.length === 0) {
    return (
      <p className="px-4 py-6 text-sm text-muted" data-testid="media-gallery-empty">
        {emptyLabel}
      </p>
    );
  }

  return (
    <div className="h-full overflow-y-auto px-4 py-3" data-testid="media-gallery">
      {media.length > 0 && (
        <div className="grid grid-cols-[repeat(auto-fill,minmax(8rem,1fr))] gap-1">
          {media.map((attachment, index) => (
            <GalleryTile
              key={attachment.id}
              attachment={attachment}
              isTerminal={isTerminal}
              onOpen={() => setViewerIndex(index)}
            />
          ))}
        </div>
      )}

      {files.length > 0 && (
        <div className="mt-4 flex flex-col gap-2">
          <h2 className="text-xs font-medium uppercase tracking-widest text-muted">
            {t("media.filesHeading")}
          </h2>
          {files.map((attachment) => (
            <AttachmentDisplay key={attachment.id} attachment={attachment} />
          ))}
        </div>
      )}

      {viewerIndex !== null && media[viewerIndex] ? (
        <GalleryViewer
          attachment={media[viewerIndex]}
          position={t("media.position", {
            index: viewerIndex + 1,
            total: media.length,
          })}
          hasMultiple={media.length > 1}
          onStep={step}
          onClose={() => setViewerIndex(null)}
        />
      ) : null}
    </div>
  );
};

interface GalleryTileProps {
  attachment: MessageAttachment;
  isTerminal: boolean;
  onOpen: () => void;
}

/** One square tile. Same resolution path as the right panel's `MediaTile`,
 * plus a click target and a play badge for videos. */
const GalleryTile: React.FC<GalleryTileProps> = ({
  attachment,
  isTerminal,
  onOpen,
}) => {
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const isVideo = attachment.content_type.startsWith("video/");
  const isPending = !attachment.object_key;

  useEffect(() => {
    if (isPending || url || failed) {
      return;
    }
    let mounted = true;
    getMediaUrl(attachment.object_key, attachment.content_hash, attachment.content_type)
      .then((resolved) => {
        if (mounted) {
          setUrl(resolved);
        }
      })
      .catch(() => {
        if (mounted) {
          setFailed(true);
        }
      });
    return () => {
      mounted = false;
    };
  }, [isPending, url, failed, attachment.object_key, attachment.content_hash, attachment.content_type]);

  return (
    <button
      type="button"
      onClick={onOpen}
      title={attachment.filename}
      data-testid="media-gallery-tile"
      className={`relative aspect-square overflow-hidden border border-line bg-surface-raised ${
        isTerminal ? "" : "rounded"
      }`}
      style={{ cursor: "zoom-in" }}
    >
      {url && !isVideo ? (
        <img
          src={url}
          alt={attachment.filename}
          loading="lazy"
          className="h-full w-full object-cover"
          onError={() => setFailed(true)}
        />
      ) : url && isVideo ? (
        <video
          src={url}
          muted
          preload="metadata"
          className="h-full w-full object-cover"
          onError={() => setFailed(true)}
        />
      ) : (
        <div className="flex h-full w-full items-center justify-center text-muted">
          <FileIcon size={20} aria-hidden />
        </div>
      )}
      {isVideo && (
        <span className="absolute inset-0 flex items-center justify-center">
          <span className="flex h-8 w-8 items-center justify-center rounded-full bg-black/60 text-white">
            <Play size={16} aria-hidden fill="currentColor" />
          </span>
        </span>
      )}
    </button>
  );
};

interface GalleryViewerProps {
  attachment: MessageAttachment;
  /** "3 / 12" positional label, already translated. */
  position: string;
  hasMultiple: boolean;
  onStep: (delta: number) => void;
  onClose: () => void;
}

/** The full-screen roll viewer — the `AttachmentDisplay` lightbox shape with
 * walk controls. */
const GalleryViewer: React.FC<GalleryViewerProps> = ({
  attachment,
  position,
  hasMultiple,
  onStep,
  onClose,
}) => {
  const { t } = useTranslation("vault");
  const [url, setUrl] = useState<string | null>(null);
  const isVideo = attachment.content_type.startsWith("video/");

  useEffect(() => {
    let mounted = true;
    setUrl(null);
    getMediaUrl(attachment.object_key, attachment.content_hash, attachment.content_type)
      .then((resolved) => {
        if (mounted) {
          setUrl(resolved);
        }
      })
      .catch(() => {
        /* the empty frame + filename below is the failure state */
      });
    return () => {
      mounted = false;
    };
  }, [attachment.object_key, attachment.content_hash, attachment.content_type]);

  return (
    <div
      data-testid="media-gallery-viewer"
      onClick={onClose}
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 9999,
        background: "rgba(0,0,0,0.92)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      <div className="absolute start-4 top-3 text-xs text-white/70">
        <span className="me-3">{position}</span>
        <span>{attachment.filename}</span>
      </div>
      <button
        type="button"
        onClick={onClose}
        aria-label={t("media.close")}
        className="absolute end-4 top-3 text-white/70 transition-colors hover:text-white"
      >
        <X size={22} aria-hidden />
      </button>

      {hasMultiple && (
        <button
          type="button"
          data-testid="media-gallery-prev"
          onClick={(event) => {
            event.stopPropagation();
            onStep(-1);
          }}
          aria-label={t("media.previous")}
          className="absolute start-3 top-1/2 -translate-y-1/2 text-white/70 transition-colors hover:text-white"
        >
          <ChevronLeft size={32} aria-hidden className="rtl-mirror" />
        </button>
      )}

      <div onClick={(event) => event.stopPropagation()}>
        {url && isVideo ? (
          // eslint-disable-next-line jsx-a11y/media-has-caption
          <video
            src={url}
            controls
            autoFocus
            style={{ maxWidth: "90vw", maxHeight: "85vh" }}
          />
        ) : url ? (
          <img
            src={url}
            alt={attachment.filename}
            style={{ maxWidth: "90vw", maxHeight: "85vh", objectFit: "contain" }}
          />
        ) : (
          <div className="p-8 text-sm text-white/70">{attachment.filename}</div>
        )}
      </div>

      {hasMultiple && (
        <button
          type="button"
          data-testid="media-gallery-next"
          onClick={(event) => {
            event.stopPropagation();
            onStep(1);
          }}
          aria-label={t("media.next")}
          className="absolute end-3 top-1/2 -translate-y-1/2 text-white/70 transition-colors hover:text-white"
        >
          <ChevronRight size={32} aria-hidden className="rtl-mirror" />
        </button>
      )}
    </div>
  );
};
