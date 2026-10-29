/** Open an external URL; the http(s) allow-list is `shell:allow-open`. */

export async function shellOpen(url: string): Promise<void> {
  const mod = await import("@tauri-apps/plugin-shell");
  await mod.open(url);
}
