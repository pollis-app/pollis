/**
 * File-dialog bridge — `dialogOpen` / `dialogSave` route to the OS picker.
 *
 * Opts shape matches Tauri's plugin-dialog so call sites don't need to be
 * rewritten:
 *   open: { multiple?, directory?, title?, defaultPath?, filters? }
 *   save: { defaultPath?, title?, filters? }
 *   filters: Array<{ name: string; extensions: string[] }>
 *
 * Both return the picked absolute path(s), or null on cancel.
 */

import { electron, hasElectron } from "./runtime";

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
  if (hasElectron()) {
    return electron().dialogOpen(opts);
  }
  const mod = await import("@tauri-apps/plugin-dialog");
  // Cast: Tauri returns `string | string[] | null` depending on multiple.
  return mod.open(opts as never) as Promise<string | string[] | null>;
}

export async function dialogSave(
  opts?: SaveDialogOptions,
): Promise<string | null> {
  if (hasElectron()) {
    return electron().dialogSave(opts);
  }
  const mod = await import("@tauri-apps/plugin-dialog");
  return mod.save(opts as never) as Promise<string | null>;
}
