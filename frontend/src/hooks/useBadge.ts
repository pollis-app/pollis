import { useEffect } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Image } from '@tauri-apps/api/image';
import { useAppStore } from '../stores/appStore';
import { useTauriReady } from './useTauriReady';

// Detect Windows once at module load — navigator.userAgent reflects the host OS in Tauri.
const IS_WINDOWS = typeof navigator !== 'undefined' &&
  navigator.userAgent.toLowerCase().includes('windows');

// Lazy-loaded icon images for Windows taskbar swap.
// Cached after the first fetch so repeated badge changes don't re-fetch.
let windowsDefaultIcon: Image | null = null;
let windowsNotifIcon: Image | null = null;

async function loadWindowsIcon(url: string): Promise<Image> {
  const res = await fetch(url);
  const bytes = new Uint8Array(await res.arrayBuffer());
  return Image.fromBytes(bytes);
}

async function getWindowsDefaultIcon(): Promise<Image> {
  if (!windowsDefaultIcon) {
    windowsDefaultIcon = await loadWindowsIcon('/windows-icon-default.png');
  }
  return windowsDefaultIcon;
}

async function getWindowsNotifIcon(): Promise<Image> {
  if (!windowsNotifIcon) {
    windowsNotifIcon = await loadWindowsIcon('/windows-icon-notification.png');
  }
  return windowsNotifIcon;
}

/**
 * Syncs the total unread message count to the OS dock/taskbar badge.
 *
 * - macOS:   Dock badge via setBadgeCount()
 * - Linux:   Taskbar badge via setBadgeCount() (GNOME, KDE, XFCE via freedesktop D-Bus)
 * - Windows: Taskbar icon swap — replaces the window icon with a variant that
 *            has a built-in red dot indicator when there are unread messages.
 *
 * Call once from AppShell.
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

    if (IS_WINDOWS) {
      const iconPromise = total > 0 ? getWindowsNotifIcon() : getWindowsDefaultIcon();
      iconPromise
        .then((img) => win.setIcon(img))
        .catch((err) => { console.warn('[badge] setIcon failed:', err); });
    } else {
      // undefined clears the badge; a number sets it
      win.setBadgeCount(total > 0 ? total : undefined).catch((err) => {
        console.warn('[badge] setBadgeCount failed:', err);
      });
    }
  }, [isReady, total]);

  // Restore the default icon/clear badge on unmount (logout / app teardown)
  useEffect(() => {
    if (!isReady) {
      return;
    }
    return () => {
      const win = getCurrentWindow();
      if (IS_WINDOWS) {
        getWindowsDefaultIcon()
          .then((img) => win.setIcon(img))
          .catch(() => {});
      } else {
        win.setBadgeCount(undefined).catch(() => {});
      }
    };
  }, [isReady]);
}
