/**
 * File-dialog bridge — `dialogOpen` / `dialogSave` route to the OS picker.
 *
 * Both are the one-line `invoke` the plugin's own `open()` / `save()` make,
 * issued through OUR `invoke` rather than by importing
 * `@tauri-apps/plugin-dialog`. Under Playwright that matters: vite
 * pre-bundles the plugin, and the alias inside that bundle resolves to a
 * SECOND copy of the IPC mock with its own store and counters, so a picker
 * result and the export it drives would be recorded in different worlds.
 *
 * Opts shape matches Tauri's plugin-dialog so call sites don't need to be
 * rewritten:
 *   open: { multiple?, directory?, title?, defaultPath?, filters? }
 *   save: { defaultPath?, title?, filters? }
 *   filters: Array<{ name: string; extensions: string[] }>
 *
 * Both return the picked absolute path(s), or null on cancel.
 */

import { invoke } from "./invoke";

export interface DialogFilter {
  name: string;
  extensions: string[];
}

export interface OpenDialogOptions {
  multiple?: boolean;
  directory?: boolean;
  title?: string;
  defaultPath?: string;
  filters?: DialogFilter[];
}

export interface SaveDialogOptions {
  title?: string;
  defaultPath?: string;
  filters?: DialogFilter[];
}

export async function dialogOpen(
  opts?: OpenDialogOptions,
): Promise<string | string[] | null> {
  // Tauri returns `string | string[] | null` depending on `multiple`.
  return invoke<string | string[] | null>("plugin:dialog|open", { options: opts ?? {} });
}

export async function dialogSave(
  opts?: SaveDialogOptions,
): Promise<string | null> {
  return invoke<string | null>("plugin:dialog|save", { options: opts ?? {} });
}
