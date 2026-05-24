import { contextBridge, ipcRenderer } from "electron";

contextBridge.exposeInMainWorld("electronAPI", {
  invoke: <T,>(cmd: string, args?: Record<string, unknown>) =>
    ipcRenderer.invoke(cmd, args) as Promise<T>,
  on: (event: string, handler: (payload: unknown) => void) => {
    const listener = (_e: unknown, payload: unknown) => handler(payload);
    ipcRenderer.on(event, listener);
    return () => ipcRenderer.removeListener(event, listener);
  },
});
