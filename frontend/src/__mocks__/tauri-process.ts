/** Browser mock for @tauri-apps/plugin-process (VITE_PLAYWRIGHT): records instead of acting. */

type Recorder = { __pollisRelaunched?: number };

export async function relaunch(): Promise<void> {
  const w = window as unknown as Recorder;
  w.__pollisRelaunched = (w.__pollisRelaunched ?? 0) + 1;
}

export async function exit(): Promise<void> {}
