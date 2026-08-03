/**
 * Shell bridge — `shellOpen(url)` opens an external URL in the OS browser.
 *
 * Renamed from Tauri's `open` to avoid the obvious name collision with
 * dialog's `open`. The Tauri plugin enforces the http(s) allow-list via
 * capabilities (`shell:allow-open` in `src-tauri/capabilities/`).
 */

export async function shellOpen(url: string): Promise<void> {
  const mod = await import("@tauri-apps/plugin-shell");
  await mod.open(url);
}
