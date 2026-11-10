/**
 * Tauri runtime detection helpers.
 * In Tauri, invoke() is always available — no polling needed.
 */
export function useTauriReady() {
  return { isDesktop: true, isReady: true };
}
