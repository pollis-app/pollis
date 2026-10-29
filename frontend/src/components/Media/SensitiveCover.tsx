import React from "react";
import { EyeOff } from "lucide-react";
import { BlurhashCanvas } from "./BlurhashCanvas";

/**
 * What a sensitive attachment (#1568) shows while it is hidden: its blurhash,
 * with one eye icon centred on it — the same in the composer, the message,
 * the media grid and the gallery. With no blurhash (a video whose poster could
 * not be read) it is a plain surface with the same eye, never the media.
 *
 * Purely visual: it fills its parent, and the parent owns the reveal.
 */
export const SensitiveCover: React.FC<{ blurhash?: string }> = ({ blurhash }) => (
  <div
    data-testid="sensitive-cover"
    className="absolute inset-0 flex items-center justify-center overflow-hidden bg-surface-high"
  >
    {blurhash && (
      <div className="absolute inset-0">
        <BlurhashCanvas hash={blurhash} />
      </div>
    )}
    <span className="relative flex h-8 w-8 items-center justify-center rounded-full bg-black/60 text-white">
      <EyeOff size={16} aria-hidden="true" />
    </span>
  </div>
);
