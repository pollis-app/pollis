// A local video's still frame (#1585, #1837) as base64 JPEG, from the platform decoder
// (`lib/media/videoFrame.ts`). `null` while it decodes, for no uri, or when it cannot.

import { useEffect, useState } from "react";
import { videoFrame } from "../lib/media/videoFrame";

export function useVideoPoster(uri: string | null): string | null {
  const [frame, setFrame] = useState<{ uri: string; b64: string } | null>(null);

  useEffect(() => {
    if (!uri) {
      return;
    }
    let active = true;
    void videoFrame(uri).then((b64) => {
      if (active && b64) {
        setFrame({ uri, b64 });
      }
    });
    return () => {
      active = false;
    };
  }, [uri]);

  // A frame decoded for an earlier uri never shows for this one.
  return frame && frame.uri === uri ? frame.b64 : null;
}
