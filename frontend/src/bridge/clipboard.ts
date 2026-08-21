/**
 * Clipboard bridge — the custom `#[tauri::command]`s `read_clipboard_files`,
 * `read_clipboard_image` and `write_clipboard_text` (implemented in
 * `src-tauri/src/lib.rs`) get bridge wrappers so callers don't reach past the
 * bridge to `invoke` them directly.
 */

import { invoke } from "./invoke";

export async function readClipboardFiles(): Promise<string[]> {
  return invoke<string[]>("read_clipboard_files");
}

/**
 * Raster image on the OS clipboard, as PNG bytes — or null when there is none.
 *
 * Bytes, not a path. The command used to save the PNG into the OS temp
 * directory and hand back its location, and nothing ever deleted the file, so
 * every screenshot a user pasted stayed on disk in the clear. The caller turns
 * these bytes into a `File` and stages them with `stageAttachment`, which keeps
 * them in the backend's memory instead.
 */
export async function readClipboardImage(): Promise<Uint8Array<ArrayBuffer> | null> {
  // The Tauri command answers with an empty body when the clipboard holds no
  // image; the raw response arrives as an ArrayBuffer.
  const bytes = await invoke<ArrayBuffer | Uint8Array | number[]>(
    "read_clipboard_image",
  );
  const array: Uint8Array<ArrayBuffer> =
    bytes instanceof Uint8Array
      ? new Uint8Array(bytes)
      : new Uint8Array(bytes as ArrayBuffer);
  return array.byteLength > 0 ? array : null;
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
