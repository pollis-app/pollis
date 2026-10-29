/** Version, relaunch, exit. No temp-dir accessor and no `convertFileSrc`, by design (temp files, asset protocol). */

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
