import React from "react";
import { EyeOff } from "lucide-react";
import { BlurhashCanvas } from "./BlurhashCanvas";

/** A hidden sensitive attachment (#1568): its blurhash (or plain surface) with an eye; the parent owns the reveal. */
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
