/** In Tauri, invoke() is always available. */
export function useTauriReady() {
  return { isDesktop: true, isReady: true };
}

export function checkIsDesktop(): boolean {
  return true;
}
