/** Browser mock for @tauri-apps/plugin-updater (VITE_PLAYWRIGHT); behaviour from `window.__POLLIS_UPDATER__`. */

type Mode = "none" | "succeed" | "fail";

type DownloadEvent =
  | { event: "Started"; data: { contentLength?: number } }
  | { event: "Progress"; data: { chunkLength: number } }
  | { event: "Finished"; data: Record<string, never> };

function mode(): Mode {
  return (window as unknown as { __POLLIS_UPDATER__?: Mode }).__POLLIS_UPDATER__ ?? "none";
}

/** `none`: no update; `succeed`: one that installs; `fail`: one whose download fails. */
export async function check(): Promise<unknown> {
  const m = mode();
  if (m === "none") {
    return null;
  }
  return {
    version: "9.9.9",
    async downloadAndInstall(progress?: (e: DownloadEvent) => void): Promise<void> {
      progress?.({ event: "Started", data: { contentLength: 100 } });
      progress?.({ event: "Progress", data: { chunkLength: 50 } });
      // Slow enough that a spec can see the busy state.
      await new Promise((r) => setTimeout(r, 300));
      if (m === "fail") {
        throw new Error("error sending request for url (https://cdn.pollis.com/releases/update.json)");
      }
      progress?.({ event: "Progress", data: { chunkLength: 50 } });
      progress?.({ event: "Finished", data: {} });
    },
  };
}
