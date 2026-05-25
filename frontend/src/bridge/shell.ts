/**
 * Shell bridge — `shellOpen(url)` opens an external URL in the OS browser.
 *
 * Renamed from Tauri's `open` to avoid the obvious name collision with
 * dialog's `open`. Validates http(s) only on the Electron path (the Tauri
 * plugin enforces this via capabilities).
 */

import { electron, hasElectron } from "./runtime";

export async function shellOpen(url: string): Promise<void> {
  if (hasElectron()) {
    await electron().shellOpenExternal(url);
    return;
  }
  const mod = await import("@tauri-apps/plugin-shell");
  await mod.open(url);
}
