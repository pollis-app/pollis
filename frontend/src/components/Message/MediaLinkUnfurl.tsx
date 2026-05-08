import React, { useCallback, useMemo, useState } from "react";
import { open } from "@tauri-apps/plugin-shell";

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

  return (
    <div data-testid="media-link-unfurl" className="mt-2 flex flex-col gap-2">
      {visible.map((link) => {
        const href = ensureProtocol(link.url);
        if (link.kind === "image") {
          return (
            <button
              key={link.url}
              type="button"
              onClick={() => handleClick(link.url)}
              className="block max-w-sm cursor-pointer p-0 bg-transparent border-0"
              title={href}
              aria-label={`Open ${href}`}
            >
              <img
                src={href}
                alt=""
                onError={() =>
                  setHidden((prev) => {
                    const next = new Set(prev);
                    next.add(link.url);
                    return next;
                  })
                }
                className="max-w-full max-h-80 rounded"
                style={{ border: "1px solid var(--c-border)" }}
              />
            </button>
          );
        }
        return (
          <video
            key={link.url}
            src={href}
            controls
            preload="metadata"
            onError={() =>
              setHidden((prev) => {
                const next = new Set(prev);
                next.add(link.url);
                return next;
              })
            }
            className="max-w-sm max-h-80 rounded"
            style={{ border: "1px solid var(--c-border)" }}
          />
        );
      })}
    </div>
  );
};
