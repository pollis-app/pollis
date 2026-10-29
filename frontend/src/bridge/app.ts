/**
 * App / process bridge — version, relaunch, exit.
 *
 * Deliberately no temp-directory accessor (`no-plaintext-temp-files.test.ts`;
 * attachment bytes go to `bridge/staging.ts`) and no `convertFileSrc`: the
 * asset protocol is off because it would bypass the path scope. Read bytes
 * through `bridge/fs.ts`.
 */

export async function getVersion(): Promise<string> {
  const mod = await import("@tauri-apps/api/app");
  return mod.getVersion();
}

export async function relaunch(): Promise<void> {
  const mod = await import("@tauri-apps/plugin-process");
  await mod.relaunch();
}

export async function exit(code = 0): Promise<void> {
  const mod = await import("@tauri-apps/plugin-process");
  await mod.exit(code);
}
