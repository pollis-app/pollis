// Mobile media transport (issue #346) — the file-path approach that
// replaces desktop's loopback HTTP media server on iOS/Android.
//
//   import { resolveMediaUri } from "../lib/media";
//   import { useMediaUri } from "../hooks/useMediaUri";
//   import { MediaImage } from "../components/Media";

export {
  resolveMediaUri,
  releaseMediaUri,
  clearMediaCache,
  MEDIA_DIR,
} from "./cache";
export { registerMediaMock } from "./mock";
