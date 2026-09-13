/**
 * App / process bridge — version, relaunch, exit.
 *
 * Delegates to `@tauri-apps/api/app` and `@tauri-apps/plugin-process`.
 *
 * There is deliberately no temp-directory accessor. One existed for a single
 * caller, which wrote pasted files into the OS temp directory and never removed
 * them; attachment bytes go to `bridge/staging.ts` instead, and handing the
 * renderer a temp path again would re-open that door — see
 * `frontend/tests/no-plaintext-temp-files.test.ts`.
 *
 * There is deliberately no `convertFileSrc` either. It minted `asset://` URLs
 * for native paths, and the asset protocol is switched off in
 * `src-tauri/tauri.conf.json` — a second, scope-checked way to read a file that
 * bypasses the path scope the four path-taking commands are gated on. Read the
 * bytes through `bridge/fs.ts` instead.
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
