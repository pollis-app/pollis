/** Swallow an expected best-effort failure; real IPC calls should `console.warn`. */
export function logIgnored(e: unknown): void {
  console.debug("ignored:", e);
}
