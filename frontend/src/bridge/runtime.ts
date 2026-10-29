/** Runtime host detection; the single `hasTauri()` every bridge module uses. */

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
 * True under the real Tauri runtime; false in `pnpm dev:frontend` and under
 * Playwright (vite-aliased mocks).
 */
export function hasTauri(): boolean {
  return (
    typeof window !== "undefined" &&
    (window as Window).__TAURI_INTERNALS__ !== undefined
  );
}
