// Outgoing video resolution ceilings (#1902): the presets for the screen share and the camera, as
// a frame height, with `0` meaning the source's own size. Pure, so the unit tests can load it.

/** Screen-share presets; `0` = the display or window's own size. */
export const SCREEN_SHARE_RESOLUTION_OPTIONS = [720, 1080, 1440, 0] as const;
export const SCREEN_SHARE_RESOLUTION_DEFAULT = 1080;

/** Camera presets; `0` = the camera's own size, the default (a webcam is rarely past 1080p). */
export const CAMERA_RESOLUTION_OPTIONS = [360, 720, 1080, 0] as const;
export const CAMERA_RESOLUTION_DEFAULT = 0;

/** Snap a stored height to the nearest preset; `0` stays "source", anything invalid is `fallback`. */
export function snapToResolutionPreset(
  v: number | undefined,
  options: readonly number[],
  fallback: number,
): number {
  if (v === undefined || !Number.isFinite(v) || v < 0) {
    return fallback;
  }
  if (v === 0) {
    return 0;
  }
  return options
    .filter((opt) => opt !== 0)
    .reduce((best, opt) => (Math.abs(opt - v) < Math.abs(best - v) ? opt : best));
}

export function clampScreenShareResolution(v: number | undefined): number {
  return snapToResolutionPreset(v, SCREEN_SHARE_RESOLUTION_OPTIONS, SCREEN_SHARE_RESOLUTION_DEFAULT);
}

export function clampCameraResolution(v: number | undefined): number {
  return snapToResolutionPreset(v, CAMERA_RESOLUTION_OPTIONS, CAMERA_RESOLUTION_DEFAULT);
}
