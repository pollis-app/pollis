import { useEffect } from 'react';
import { getCurrentWindow, Image, setTrayUnread, type PollisImage } from '../bridge';
import { appStore } from '../stores/appStore';
import { useObserver } from 'mobx-react-lite';
import { useTauriReady } from './useTauriReady';
import { isMac, isWindows } from '../utils/platform';
import { logIgnored } from '../utils/log';

// Windows taskbar icons, fetched once.
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

/** Sync unread to the dock/taskbar badge (Windows swaps in a red-dot icon). Call once from AppShell. */
export function useBadge() {
  const { isReady } = useTauriReady();
  const total = useObserver(() => appStore.totalUnread);

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
      win.setBadgeCount(total > 0 ? total : undefined).catch((err) => {
        console.warn('[badge] setBadgeCount failed:', err);
      });
    }

    if (!isMac) {
      void setTrayUnread(total).catch((err) => {
        console.warn('[tray] setTrayUnread failed:', err);
      });
    }
  }, [isReady, total]);

  useEffect(() => {
    if (!isReady) {
      return;
    }
    return () => {
      const win = getCurrentWindow();
      if (isWindows) {
        getWindowsDefaultIcon()
          .then((img) => win.setIcon(img))
          .catch(logIgnored);
      } else {
        win.setBadgeCount(undefined).catch(logIgnored);
      }
    };
  }, [isReady]);
}
