/**
 * File-dialog bridge — `dialogOpen` / `dialogSave` route to the OS picker.
 *
 * These no longer invoke the dialog plugin's own IPC commands. Its `dialog:*`
 * ACL permissions are gone from `src-tauri/capabilities/default.json` and the
 * renderer cannot reach the plugin at all; `pick_open_paths` / `pick_save_path`
 * drive the same OS picker from Rust instead.
 *
 * The reason is not the dialog — it is what happens to the path afterwards.
 * `upload_media`, `upload_group_emoji`, `export_archive` and
 * `fetch_export_attachments` all take a path off the IPC and act on it with the
 * app's full authority, so Rust records what the picker returned and refuses
 * anything else (`src-tauri/src/pathscope.rs`). Recording has to happen where
 * the path is produced: a "now register this path for me" call the renderer
 * makes would be a call a compromised renderer makes with `~/.ssh/id_rsa`.
 *
 * Opts shape is unchanged, so call sites did not have to be rewritten:
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
  const options = opts ?? {};
  // Rust always answers with a list — one shape there means one code path
  // recording the grants. Narrow it back to what the call sites expect.
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
