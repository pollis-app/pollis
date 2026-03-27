import { useEffect } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useAppStore } from '../stores/appStore';
import { useTauriReady } from './useTauriReady';

/**
 * Syncs the total unread message count to the OS dock/taskbar badge.
 * Call once from AppShell — it subscribes to the Zustand store and
 * updates the badge whenever unreadCounts changes.
 */
export function useBadge() {
  const { isReady } = useTauriReady();
  const unreadCounts = useAppStore((s) => s.unreadCounts);

  const total = Object.values(unreadCounts).reduce((sum, n) => sum + n, 0);

  useEffect(() => {
    if (!isReady) {
      return;
    }

    const win = getCurrentWindow();
    // null clears the badge; a number sets it
    win.setBadgeCount(total > 0 ? total : null).catch((err) => {
      console.warn('[badge] setBadgeCount failed:', err);
    });
  }, [isReady, total]);

  // Clear badge on unmount (logout / app teardown)
  useEffect(() => {
    if (!isReady) {
      return;
    }
    return () => {
      getCurrentWindow().setBadgeCount(null).catch(() => {});
    };
  }, [isReady]);
}
