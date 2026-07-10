// Webcam camera contract types — the renderer-side mirror of the Rust
// structs the backend serializes. Kept in sync with:
//   - `pollis_capture_proto::CameraSource` / `CameraList`
//   - `pollis_core::commands::camera::CameraEvent`
//
// The control-plane commands these flow through (registered in
// `src-tauri/src/lib.rs`):
//   - `list_video_devices()            -> CameraList`
//   - `start_camera({ deviceId })      -> void`
//   - `stop_camera()                   -> void`
//   - `subscribe_camera_events(onEvent)`  // Channel<CameraEvent>
//
// Camera publishes a third video track (`TrackSource::Camera`) into the
// active voice room alongside the mic and screen share; room-level E2EE
// encrypts it automatically. Capture is live on macOS, Linux, and Windows;
// unsupported platforms return a "not yet supported" error from the backend.

/** A capturable video device. Mirrors `pollis_capture_proto::CameraSource`.
 *  `id` is an opaque, stable per-platform handle (macOS
 *  `AVCaptureDevice.uniqueID`, Linux V4L2 node path, Windows MF symbolic
 *  link), echoed back verbatim to `start_camera`. */
export interface CameraSource {
  id: string;
  name: string;
}

/** Enumeration result from `list_video_devices`. Mirrors
 *  `pollis_capture_proto::CameraList`. Lists every device the OS reports —
 *  no virtual-camera filtering (Discord/Zoom convention). */
export interface CameraList {
  cameras: CameraSource[];
}

/** Local-camera lifecycle events. Mirrors
 *  `pollis_core::commands::camera::CameraEvent`. Remote camera tiles are
 *  driven by the LiveKit view client reading `TrackSource::Camera`, so
 *  there are intentionally no `remote_*` variants here. */
export type CameraEvent =
  | { type: "local_started"; width: number; height: number }
  | { type: "local_stopped" }
  | { type: "local_error"; message: string };
