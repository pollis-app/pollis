/**
 * Clipboard bridge — the two custom `#[tauri::command]`s
 * `read_clipboard_files` and `read_clipboard_image_to_temp` (implemented in
 * `src-tauri/src/lib.rs`) get bridge wrappers so callers don't reach past the
 * bridge to `invoke` them directly.
 */

import { invoke } from "./invoke";

export async function readClipboardFiles(): Promise<string[]> {
  return invoke<string[]>("read_clipboard_files");
}

export async function readClipboardImageToTemp(): Promise<string | null> {
  // The Tauri command returns an empty string when no image is on the
  // clipboard. Normalise to null.
  const path = await invoke<string>("read_clipboard_image_to_temp");
  return path && path.length > 0 ? path : null;
}
