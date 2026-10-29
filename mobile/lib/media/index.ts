// Mobile media transport (#346), replacing desktop's loopback media server.

export {
  resolveMediaUri,
  releaseMediaUri,
  resolveNamedMediaUri,
  releaseNamedMediaUri,
  clearMediaCache,
  mediaCacheGeneration,
  MEDIA_DIR,
} from "./cache";
export { registerMediaMock } from "./mock";
