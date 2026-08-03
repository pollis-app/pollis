/**
 * Filesystem bridge — narrow subset of `@tauri-apps/plugin-fs`.
 *
 * Only the three calls the renderer actually uses today are exposed:
 *   - writeFile(path, bytes): save a downloaded attachment / drop a paste
 *     into temp dir before `send_message` reads it.
 *   - readFile(path) -> bytes: image/video preview pre-send.
 *   - stat(path) -> { size, isFile, isDirectory, modifiedAtMs }: filter
 *     directories out of dropped paths before treating them as files.
 *
 * All paths are user-chosen (via dialog) or in the OS temp dir.
 */

export async function writeFile(
  path: string,
  bytes: Uint8Array,
): Promise<void> {
  const mod = await import("@tauri-apps/plugin-fs");
  await mod.writeFile(path, bytes);
}

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
