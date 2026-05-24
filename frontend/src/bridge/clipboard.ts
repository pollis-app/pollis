/**
 * Clipboard bridge — the two custom Tauri IPCs `read_clipboard_files` and
 * `read_clipboard_image_to_temp` get bridge wrappers so callers don't have
 * to runtime-branch.
 *
 * Under Tauri the existing `#[tauri::command]`s in `src-tauri/src/lib.rs`
 * remain the source of truth (and stay there until Phase 8 cleanup). Under
 * Electron we route to the preload's `clipboardRead*` helpers, which the
 * main process implements using the same osascript-on-mac /
 * `text/uri-list`-on-linux/windows split.
 */

import { electron, hasElectron } from "./runtime";
import { invoke } from "./invoke";

export async function readClipboardFiles(): Promise<string[]> {
  if (hasElectron()) {
    return electron().clipboardReadFiles();
  }
  return invoke<string[]>("read_clipboard_files");
}

export async function readClipboardImageToTemp(): Promise<string | null> {
  if (hasElectron()) {
    return electron().clipboardReadImageToTemp();
  }
  // Tauri command returns an empty string when no image is on the
  // clipboard. Normalise to null so both runtimes return the same shape.
  const path = await invoke<string>("read_clipboard_image_to_temp");
  return path && path.length > 0 ? path : null;
}
