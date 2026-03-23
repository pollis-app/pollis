import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalSize, LogicalPosition } from "@tauri-apps/api/dpi";

const STORAGE_KEY = "pollis-window-state";

interface WindowState {
  width: number;
  height: number;
  x: number;
  y: number;
}

const MIN_WIDTH = 420;
const MIN_HEIGHT = 360;

function isValidWindowState(s: unknown): s is WindowState {
  if (!s || typeof s !== "object") {
    return false;
  }
  const ws = s as Record<string, unknown>;
  return (
    typeof ws.width === "number" &&
    ws.width >= MIN_WIDTH &&
    ws.width <= 8192 &&
    typeof ws.height === "number" &&
    ws.height >= MIN_HEIGHT &&
    ws.height <= 8192 &&
    typeof ws.x === "number" &&
    ws.x >= -200 &&
    typeof ws.y === "number" &&
    ws.y >= 0
  );
}

export async function restoreWindowState(): Promise<void> {
  try {
    const appWindow = getCurrentWindow();
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) {
      await appWindow.center();
      return;
    }
    const parsed: unknown = JSON.parse(raw);
    if (!isValidWindowState(parsed)) {
      await appWindow.center();
      return;
    }
    await appWindow.setSize(new LogicalSize(parsed.width, parsed.height));
    await appWindow.setPosition(new LogicalPosition(parsed.x, parsed.y));
  } catch {
    // Best-effort — ignore failures
  }
}

export function useWindowState(): void {
  useEffect(() => {
    const appWindow = getCurrentWindow();
    let saveTimeout: ReturnType<typeof setTimeout>;

    const save = async () => {
      try {
        const [size, position, scale] = await Promise.all([
          appWindow.innerSize(),
          appWindow.outerPosition(),
          appWindow.scaleFactor(),
        ]);
        const state: WindowState = {
          width: Math.round(size.width / scale),
          height: Math.round(size.height / scale),
          x: Math.round(position.x / scale),
          y: Math.round(position.y / scale),
        };
        localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
      } catch {
        // Ignore
      }
    };

    const schedulesSave = () => {
      clearTimeout(saveTimeout);
      saveTimeout = setTimeout(save, 500);
    };

    let unlistenResize: (() => void) | undefined;
    let unlistenMove: (() => void) | undefined;

    const setup = async () => {
      unlistenResize = await appWindow.onResized(schedulesSave);
      unlistenMove = await appWindow.onMoved(schedulesSave);
    };

    setup();

    return () => {
      clearTimeout(saveTimeout);
      unlistenResize?.();
      unlistenMove?.();
    };
  }, []);
}
