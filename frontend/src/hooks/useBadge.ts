import { useEffect } from 'react';
import { getCurrentWindow, Image, type PollisImage } from '../bridge';
import { useAppStore } from '../stores/appStore';
import { useTauriReady } from './useTauriReady';
import { isWindows } from '../utils/platform';

// Lazy-loaded icon images for Windows taskbar swap.
// Cached after the first fetch so repeated badge changes don't re-fetch.
let windowsDefaultIcon: PollisImage | null = null;
let windowsNotifIcon: PollisImage | null = null;

async function loadWindowsIcon(url: string): Promise<PollisImage> {
  const res = await fetch(url);
  const bytes = new Uint8Array(await res.arrayBuffer());
  return Image.fromBytes(bytes);
}

async function getWindowsDefaultIcon(): Promise<PollisImage> {
  if (!windowsDefaultIcon) {
    windowsDefaultIcon = await loadWindowsIcon('/windows-icon-default.png');
  }
  return windowsDefaultIcon;
}

async function getWindowsNotifIcon(): Promise<PollisImage> {
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

    if (isWindows) {
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
      if (isWindows) {
        getWindowsDefaultIcon()
          .then((img) => win.setIcon(img))
          .catch(() => {});
      } else {
        win.setBadgeCount(undefined).catch(() => {});
      }
    };
  }, [isReady]);
}
