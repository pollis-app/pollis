import { contextBridge, ipcRenderer } from "electron";

// All command IPC routes through a single "invoke" channel — main process
// dispatches by name into pollis-node, same shape as Tauri's invoke_handler.
// Adding a new command is a one-line match arm in pollis-node/src/dispatch/;
// nothing here changes.
//
// Channel<T>-based subscriptions (voice/screenshare/realtime/terminal-output
// events) fan in through a single Rust → Node ThreadsafeFunction registered
// once at startup, then main forwards each envelope to renderers via
// `webContents.send("channel:<id>", payload)`. `channelOn` is the renderer
// side of that name.

contextBridge.exposeInMainWorld("electronAPI", {
  invoke: <T,>(cmd: string, args?: unknown, options?: unknown) =>
    ipcRenderer.invoke("invoke", cmd, args ?? null, options ?? null) as Promise<T>,
  on: (event: string, handler: (payload: unknown) => void) => {
    const listener = (_e: unknown, payload: unknown) => handler(payload);
    ipcRenderer.on(event, listener);
    return () => ipcRenderer.removeListener(event, listener);
  },
  channelOn: (channelId: string, handler: (payload: unknown) => void) => {
    const eventName = `channel:${channelId}`;
    const listener = (_e: unknown, payload: unknown) => handler(payload);
    ipcRenderer.on(eventName, listener);
    return () => ipcRenderer.removeListener(eventName, listener);
  },
});
