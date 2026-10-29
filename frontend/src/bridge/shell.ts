/**
 * Shell bridge — `shellOpen(url)` opens an external URL in the OS browser. The
 * http(s) allow-list is `shell:allow-open` in `src-tauri/capabilities/`.
 */

export async function shellOpen(url: string): Promise<void> {
  const mod = await import("@tauri-apps/plugin-shell");
  await mod.open(url);
}
