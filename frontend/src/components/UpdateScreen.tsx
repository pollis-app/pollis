import React, { useEffect, useState } from "react";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { invoke } from "@tauri-apps/api/core";
import { LoadingSpinner } from "./ui/LoaderSpinner";

type UpdatePhase = "preparing" | "checking" | "downloading" | "installing" | "relaunching" | "error";

/**
 * Fully automatic update screen. On mount it checks for an update, downloads
 * it, installs it, and relaunches — no user interaction required.
 */
export const UpdateScreen: React.FC = () => {
  const [phase, setPhase] = useState<UpdatePhase>("preparing");
  const [progress, setProgress] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    async function runUpdate() {
      try {
        // Gracefully tear down voice / realtime before the install.
        // TerminalApp has already unmounted (appState flipped to
        // update-required), which triggers useVoiceChannel / realtime
        // cleanup, but we also invoke leave_voice_channel directly to
        // guarantee LiveKit is disconnected with its 5s timeout before
        // the updater overwrites the binary.
        setPhase("preparing");
        await invoke("leave_voice_channel").catch(() => {});
        // Small settle window so any in-flight MLS commits / network
        // sends can finish before the process is replaced.
        await new Promise((r) => setTimeout(r, 300));

        if (cancelled) {
          return;
        }

        setPhase("checking");
        const update = await check();

        if (!update || cancelled) {
          return;
        }

        setPhase("downloading");
        let totalBytes = 0;
        let downloadedBytes = 0;

        await update.downloadAndInstall((event) => {
          if (cancelled) {
            return;
          }
          switch (event.event) {
            case "Started":
              totalBytes = event.data.contentLength ?? 0;
              break;
            case "Progress":
              downloadedBytes += event.data.chunkLength;
              if (totalBytes > 0) {
                setProgress(Math.round((downloadedBytes / totalBytes) * 100));
              }
              break;
            case "Finished":
              setPhase("installing");
              break;
          }
        });

        if (cancelled) {
          return;
        }

        setPhase("relaunching");
        await relaunch();
      } catch (err) {
        if (!cancelled) {
          console.error("[update] Auto-update failed:", err);
          setError(err instanceof Error ? err.message : String(err));
          setPhase("error");
        }
      }
    }

    runUpdate();
    return () => { cancelled = true; };
  }, []);

  const label = (() => {
    switch (phase) {
      case "preparing":
        return "Preparing to update…";
      case "checking":
        return "Checking for updates…";
      case "downloading":
        return progress !== null ? `Downloading update… ${progress}%` : "Downloading update…";
      case "installing":
        return "Installing update…";
      case "relaunching":
        return "Relaunching…";
      case "error":
        return `Update failed: ${error}`;
    }
  })();

  return (
    <div
      data-testid="update-screen"
      style={{
        height: "100%",
        width: "100%",
        background: "var(--c-bg)",
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        fontFamily: "var(--font-mono, monospace)",
      }}
    >
      <div
        style={{
          width: "100%",
          maxWidth: 480,
          padding: "2rem",
          display: "flex",
          flexDirection: "column",
          gap: "1.25rem",
          alignItems: "center",
        }}
      >
        <div className="flex items-center gap-2">
          {phase !== "error" && <LoadingSpinner size="sm" />}
          <span
            className="text-xs font-mono"
            style={{ color: phase === "error" ? "#ff6b6b" : "var(--c-text)" }}
          >
            {label}
          </span>
        </div>

        {phase === "downloading" && progress !== null && (
          <div
            style={{
              width: "100%",
              maxWidth: 280,
              height: 4,
              borderRadius: 2,
              background: "var(--c-border)",
              overflow: "hidden",
            }}
          >
            <div
              style={{
                width: `${progress}%`,
                height: "100%",
                background: "var(--c-accent)",
                transition: "width 0.2s ease",
              }}
            />
          </div>
        )}
      </div>
    </div>
  );
};
