import { contextBridge, ipcRenderer } from "electron";

// All command IPC routes through a single "invoke" channel — main process
// dispatches by name into pollis-node, same shape as Tauri's invoke_handler.
// Adding a new command is a one-line match arm in pollis-node/src/lib.rs;
// nothing here changes.
contextBridge.exposeInMainWorld("electronAPI", {
  invoke: <T,>(cmd: string, args?: unknown) =>
    ipcRenderer.invoke("invoke", cmd, args ?? null) as Promise<T>,
  on: (event: string, handler: (payload: unknown) => void) => {
    const listener = (_e: unknown, payload: unknown) => handler(payload);
    ipcRenderer.on(event, listener);
    return () => ipcRenderer.removeListener(event, listener);
  },
});
