// Mobile media transport (#346), replacing desktop's loopback media server.

export {
  resolveMediaUri,
  releaseMediaUri,
  resolveNamedMediaUri,
  releaseNamedMediaUri,
  clearMediaCache,
  mediaCacheGeneration,
  peekMediaUri,
  trimMediaCache,
  startMediaRetention,
  MEDIA_RETAIN_MAX,
  MEDIA_DIR,
} from "./cache";
export { registerMediaMock } from "./mock";
