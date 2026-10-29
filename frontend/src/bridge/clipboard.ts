/** Wrappers for the clipboard commands in `src-tauri/src/lib.rs`. */

import { invoke } from "./invoke";

export async function readClipboardFiles(): Promise<string[]> {
  return invoke<string[]>("read_clipboard_files");
}

/**
 * Raster image on the OS clipboard, as PNG bytes, or null. Bytes, never a temp
 * path: the caller stages them in backend memory with `stageAttachment`.
 */
export async function readClipboardImage(): Promise<Uint8Array<ArrayBuffer> | null> {
  // An empty body means no image.
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
 * Write plain text to the OS clipboard; false if the write failed. A Rust
 * command because `navigator.clipboard` is unreliable on WebKitGTK.
 */
export async function writeClipboardText(text: string): Promise<boolean> {
  return invoke<boolean>("write_clipboard_text", { text });
}
