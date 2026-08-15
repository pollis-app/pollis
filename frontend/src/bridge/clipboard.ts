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

/**
 * Write plain text to the OS clipboard. Returns false if the write failed.
 *
 * Backed by a Rust command rather than `navigator.clipboard` because the
 * latter is unreliable on WebKitGTK (the Linux webview this app ships on).
 */
export async function writeClipboardText(text: string): Promise<boolean> {
  return invoke<boolean>("write_clipboard_text", { text });
}
