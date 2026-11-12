// A video's still frame from the platform decoder (#1837): AVAssetImageGenerator on iOS,
// MediaMetadataRetriever on Android (`modules/pollis-native`: PollisVideoFrame.mm /
// PollisVideoFrameModule.kt), as base64 JPEG in memory. One path for the composer card, the
// poster a send hands the core, and a received video whose sender had no poster.

import { NativeModules } from "react-native";

type FrameModule = { frame(uri: string, maxEdge: number): Promise<string> };

/** Longest edge asked of the decoder; the core shrinks the sent poster further. */
const VIDEO_FRAME_EDGE = 640;

/** A binary without the module (older than #1837) or a file it cannot decode gives null. */
export async function videoFrame(uri: string, maxEdge = VIDEO_FRAME_EDGE): Promise<string | null> {
  const native = (NativeModules as { PollisVideoFrame?: FrameModule }).PollisVideoFrame;
  if (!native) {
    return null;
  }
  try {
    const b64 = await native.frame(uri, maxEdge);
    return b64 || null;
  } catch {
    return null;
  }
}
