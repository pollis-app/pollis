/**
 * File-dialog bridge: `dialogOpen` / `dialogSave` drive the OS picker from Rust
 * (`pick_open_paths` / `pick_save_path`); the renderer has no `dialog:*` ACL.
 *
 * Path-taking commands only accept paths a picker returned, recorded where the
 * path is produced (`src-tauri/src/pathscope.rs`): a renderer "register this
 * path" call is one a compromised renderer could make with `~/.ssh/id_rsa`.
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
  const options = opts ?? {};
  // Rust always answers with a list; narrow it back for the call sites.
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
