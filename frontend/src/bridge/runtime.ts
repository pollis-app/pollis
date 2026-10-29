/** Host detection; Tauri is the only host (#386 / #389). */

export type DragDropPayload = {
  type: "enter" | "over" | "drop" | "leave";
  paths: string[];
};

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

/** False in browser-only dev and under Playwright. */
export function hasTauri(): boolean {
  return (
    typeof window !== "undefined" &&
    (window as Window).__TAURI_INTERNALS__ !== undefined
  );
}
