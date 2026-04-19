import { useEffect } from 'react';
import { useAppStore } from '../stores/appStore';
import * as api from '../services/api';

/**
 * Event-driven network status. Pollis has no centralized backend to ping,
 * so status is reported from:
 *   - `api.getNetworkStatus()` — currently a client-side stub that returns
 *     'online' (see services/api.ts). If we add a real connectivity probe
 *     later (e.g. a Tauri command that round-trips Turso), it slots in here
 *     without changing the hook.
 *   - Browser `online`/`offline` events — best-effort; WKWebView's
 *     `navigator.onLine` is unreliable inside Tauri but the events fire.
 *
 * Deliberately does NOT run on an interval. The app has no periodic
 * polling.
 *
 * @param enabled - Whether to enable network status monitoring (default: true)
 */
export const useNetworkStatus = (enabled: boolean = true) => {
  const { networkStatus, killSwitchEnabled, setNetworkStatus, setKillSwitchEnabled } = useAppStore();

  useEffect(() => {
    if (!enabled) {
      return;
    }

    let mounted = true;

    const refresh = async () => {
      if (!mounted) {
        return;
      }
      try {
        const status = await api.getNetworkStatus();
        if (mounted) {
          setNetworkStatus(status);
        }
      } catch {
        // getNetworkStatus is currently a pure stub and can't throw, but
        // guard anyway so a future real probe failing doesn't crash the UI.
      }
    };

    const handleOnline = () => {
      if (!killSwitchEnabled) {
        void refresh();
      }
    };

    const handleOffline = () => {
      if (!killSwitchEnabled) {
        setNetworkStatus('offline');
      }
    };

    void refresh();
    window.addEventListener('online', handleOnline);
    window.addEventListener('offline', handleOffline);

    return () => {
      mounted = false;
      window.removeEventListener('online', handleOnline);
      window.removeEventListener('offline', handleOffline);
    };
  }, [enabled, killSwitchEnabled, setNetworkStatus]);

  const toggleKillSwitch = async (enabled: boolean) => {
    setKillSwitchEnabled(enabled);
    if (enabled) {
      setNetworkStatus('kill-switch');
    } else {
      const status = await api.getNetworkStatus();
      setNetworkStatus(status);
    }
  };

  return {
    networkStatus,
    killSwitchEnabled,
    toggleKillSwitch,
    isOnline: networkStatus === 'online',
  };
};

