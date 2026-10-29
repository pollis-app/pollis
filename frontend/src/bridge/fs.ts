/**
 * Filesystem bridge — read only: `readFile` (preview pre-send) and `stat`
 * (filter directories out of drops).
 *
 * Deliberately no `writeFile`: saving goes through Rust (`save_media_to_path`)
 * so the file gets the platform provenance marker (quarantine /
 * mark-of-the-web). Paths are user-chosen and recorded by
 * `src-tauri/src/pathscope.rs`.
 */

export async function readFile(path: string): Promise<Uint8Array<ArrayBuffer>> {
  const mod = await import("@tauri-apps/plugin-fs");
  return mod.readFile(path);
}

export interface FileInfo {
  size: number;
  isFile: boolean;
  isDirectory: boolean;
  modifiedAtMs: number;
}

export async function stat(path: string): Promise<FileInfo> {
  const mod = await import("@tauri-apps/plugin-fs");
  const info = await mod.stat(path);
  return {
    size: info.size,
    isFile: info.isFile,
    isDirectory: info.isDirectory,
    modifiedAtMs:
      info.mtime instanceof Date
        ? info.mtime.getTime()
        : typeof info.mtime === "number"
          ? info.mtime
          : 0,
  };
}
