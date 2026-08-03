/**
 * Runtime host detection.
 *
 * Imported by every bridge module + the top-level `bridge.ts` so the same
 * `hasTauri()` answer is used everywhere.
 *
 * There is exactly one host: Tauri (`src-tauri/`). The bridge modules used to
 * branch on a second Electron host; that shell was reverted (#386 / #389) and
 * those branches are gone.
 */

export type DragDropPayload = {
  type: "enter" | "over" | "drop" | "leave";
  paths: string[];
};

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

/**
 * True under the real Tauri runtime. False in browser-only dev
 * (`pnpm dev:frontend`) and under Playwright, where `@tauri-apps/api/*` is
 * vite-aliased to `src/__mocks__/`.
 */
export function hasTauri(): boolean {
  return (
    typeof window !== "undefined" &&
    (window as Window).__TAURI_INTERNALS__ !== undefined
  );
}
