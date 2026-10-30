/** Browser mock for @tauri-apps/plugin-shell (VITE_PLAYWRIGHT): records each URL opened. */

export async function open(url: string): Promise<void> {
  const w = window as unknown as { __pollisOpened?: string[] };
  w.__pollisOpened = [...(w.__pollisOpened ?? []), url];
}
