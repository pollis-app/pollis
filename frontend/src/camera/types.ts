// Mirrors `pollis_capture_proto::CameraSource` / `CameraList` and
// `pollis_core::commands::camera::CameraEvent`.

/** `id` is an opaque, stable per-platform handle, echoed back to `start_camera`. */
export interface CameraSource {
  id: string;
  name: string;
}

/** Every device the OS reports; no virtual-camera filtering. */
export interface CameraList {
  cameras: CameraSource[];
}

/** Local-camera lifecycle only: remote tiles read `TrackSource::Camera` via LiveKit. */
export type CameraEvent =
  | { type: "local_started"; width: number; height: number }
  | { type: "local_stopped" }
  | { type: "local_error"; message: string };
