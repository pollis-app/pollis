import React, { useCallback, useMemo, useState } from "react";
import { open } from "@tauri-apps/plugin-shell";

// Known limitation (low priority): inline previews only fire when the URL ends in
// a recognised image/video extension. Sites like Giphy/Tenor/Imgur that serve media
// behind extension-less share URLs (e.g. giphy.com/gifs/...) won't unfurl. Pasting
// the direct .gif/.mp4 URL works, and copy-pasting the actual media into the input
// also works, so we accept the gap rather than building an OG/oEmbed unfurl service.
const URL_REGEX = /(https?:\/\/[^\s<>"')\]]+|www\.[^\s<>"')\]]+\.[^\s<>"')\]]+)/gi;

const IMAGE_EXTS = ["jpg", "jpeg", "png", "gif", "webp", "avif", "bmp", "svg"];
const VIDEO_EXTS = ["mp4", "webm", "mov", "m4v", "ogv"];

type MediaKind = "image" | "video";

interface MediaLink {
  url: string;
  kind: MediaKind;
}

function ensureProtocol(url: string): string {
  if (/^https?:\/\//i.test(url)) {
    return url;
  }
  return `https://${url}`;
}

function classify(url: string): MediaKind | null {
  let pathname: string;
  try {
    pathname = new URL(ensureProtocol(url)).pathname.toLowerCase();
  } catch {
    return null;
  }
  const dot = pathname.lastIndexOf(".");
  if (dot < 0) {
    return null;
  }
  const ext = pathname.slice(dot + 1);
  if (IMAGE_EXTS.includes(ext)) {
    return "image";
  }
  if (VIDEO_EXTS.includes(ext)) {
    return "video";
  }
  return null;
}

function extractMediaLinks(text: string): MediaLink[] {
  const out: MediaLink[] = [];
  const seen = new Set<string>();
  URL_REGEX.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = URL_REGEX.exec(text)) !== null) {
    const url = match[0];
    if (seen.has(url)) {
      continue;
    }
    seen.add(url);
    const kind = classify(url);
    if (kind) {
      out.push({ url, kind });
    }
  }
  return out;
}

interface MediaLinkUnfurlProps {
  text: string;
}

export const MediaLinkUnfurl: React.FC<MediaLinkUnfurlProps> = ({ text }) => {
  const links = useMemo(() => extractMediaLinks(text), [text]);
  const [hidden, setHidden] = useState<Set<string>>(() => new Set());

  const handleClick = useCallback((url: string) => {
    open(ensureProtocol(url));
  }, []);

  const visible = links.filter((l) => !hidden.has(l.url));
  if (visible.length === 0) {
    return null;
  }

  const thumbStyle: React.CSSProperties = {
    width: 96,
    height: 96,
    objectFit: "cover",
    display: "block",
    border: "1px solid var(--c-border)",
    borderRadius: 4,
    background: "var(--c-surface-high)",
  };

  return (
    <div data-testid="media-link-unfurl" className="mt-2 flex flex-wrap gap-1">
      {visible.map((link) => {
        const href = ensureProtocol(link.url);
        const onError = () =>
          setHidden((prev) => {
            const next = new Set(prev);
            next.add(link.url);
            return next;
          });
        if (link.kind === "image") {
          return (
            <button
              key={link.url}
              type="button"
              onClick={() => handleClick(link.url)}
              className="p-0 bg-transparent border-0 cursor-pointer flex-shrink-0"
              title={href}
              aria-label={`Open ${href}`}
            >
              <img src={href} alt="" onError={onError} style={thumbStyle} />
            </button>
          );
        }
        return (
          <video
            key={link.url}
            src={href}
            controls
            preload="metadata"
            onError={onError}
            style={{ ...thumbStyle, objectFit: "cover" }}
          />
        );
      })}
    </div>
  );
};
