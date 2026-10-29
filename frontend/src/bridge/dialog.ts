/** `dialogOpen` / `dialogSave` via Rust's `pick_open_paths` / `pick_save_path`, which record the chosen paths (`src-tauri/src/pathscope.rs`). Null on cancel. */

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
  const options = opts ?? {};
  // Rust always returns a list; narrow it.
  const picked = await invoke<string[]>("pick_open_paths", { options });
  if (!picked || picked.length === 0) {
    return null;
  }
  return options.multiple ? picked : picked[0];
}

export async function dialogSave(
  opts?: SaveDialogOptions,
): Promise<string | null> {
  return invoke<string | null>("pick_save_path", { options: opts ?? {} });
}
