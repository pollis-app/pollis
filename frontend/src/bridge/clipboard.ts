/** Wrappers for the clipboard commands in `src-tauri/src/lib.rs`. */

import { invoke } from "./invoke";

export async function readClipboardFiles(): Promise<string[]> {
  return invoke<string[]>("read_clipboard_files");
}

/** Clipboard image as PNG bytes, or null; bytes, never a temp file. */
export async function readClipboardImage(): Promise<Uint8Array<ArrayBuffer> | null> {
  // Empty body = no image.
  const bytes = await invoke<ArrayBuffer | Uint8Array | number[]>(
    "read_clipboard_image",
  );
  const array: Uint8Array<ArrayBuffer> =
    bytes instanceof Uint8Array
      ? new Uint8Array(bytes)
      : new Uint8Array(bytes as ArrayBuffer);
  return array.byteLength > 0 ? array : null;
}

/** Write text; false on failure. Rust-backed since `navigator.clipboard` is unreliable on WebKitGTK. */
export async function writeClipboardText(text: string): Promise<boolean> {
  return invoke<boolean>("write_clipboard_text", { text });
}
