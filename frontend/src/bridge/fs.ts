/**
 * Filesystem bridge — narrow subset of `@tauri-apps/plugin-fs`.
 *
 * READ ONLY. Two calls, both of them reads:
 *   - readFile(path) -> bytes: image/video preview pre-send.
 *   - stat(path) -> { size, isFile, isDirectory, modifiedAtMs }: filter
 *     directories out of dropped paths before treating them as files.
 *
 * There is deliberately no `writeFile`. The renderer's one writer was "save
 * attachment as…", and that moved to Rust (`save_media_to_path`) so the saved
 * file gets this platform's provenance marker — macOS quarantine, Windows
 * mark-of-the-web — which nothing in the webview can apply. Nothing else ever
 * needed to write, so the capability went with it: `fs:allow-temp-write` is out
 * of `src-tauri/capabilities/default.json` too, and the renderer can no longer
 * put a file on the disk at all.
 *
 * Every path here is user-chosen — a picker result, or a file the OS dropped on
 * the window — and Rust records which (`src-tauri/src/pathscope.rs`).
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
