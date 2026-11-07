import { errorMessage } from "../utils/errorMessage";
import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { check, relaunch, invoke, shellOpen } from "../bridge";
import { DOWNLOAD_URL } from "./ClientBlockedScreen";
import { Button } from "./ui/Button";
import { LoadingSpinner } from "./ui/LoaderSpinner";

type UpdatePhase = "preparing" | "checking" | "downloading" | "installing" | "relaunching" | "error";

// Mirrors `ReadOnlyLocation` in src-tauri/src/commands/install_kind.rs.
type ReadOnlyLocation = "disk_image" | "translocated" | "translocated_healed";

// The updater replaces the bundle in place, so read-only mounts fail with EROFS.
function isReadOnlyError(message: string): boolean {
  return /read-only file system|os error 30/i.test(message);
}

interface Props {
  /** Leave this screen and keep running the installed build (nothing newer, or the user chose to). */
  onContinue: () => void;
}

/**
 * Checks, downloads, installs and relaunches with no interaction. A failure is never a dead end
 * (#1758): retry, download the installer, or keep using this build. The published minimum
 * version (#1611) is what refuses an unsupported build, not this screen.
 */
export const UpdateScreen: React.FC<Props> = ({ onContinue }) => {
  const { t } = useTranslation("nav");
  const [phase, setPhase] = useState<UpdatePhase>("preparing");
  const [progress, setProgress] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Bumped by Try again to re-run the whole sequence.
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    let cancelled = false;

    async function runUpdate() {
      try {
        // Also leave voice directly so LiveKit disconnects before the binary is overwritten.
        setPhase("preparing");
        await invoke("leave_voice_channel").catch((e) => console.warn("leave_voice_channel failed", e));
        // Let in-flight sends settle.
        await new Promise((r) => setTimeout(r, 300));

        if (cancelled) {
          return;
        }

        // A read-only location is sure to fail; say how to fix it.
        const location = await invoke<ReadOnlyLocation | null>("detect_read_only_location").catch(() => null);
        if (cancelled) {
          return;
        }
        if (location === "disk_image") {
          setError(t("update.readOnlyDiskImage"));
          setPhase("error");
          return;
        }
        if (location === "translocated") {
          setError(t("update.readOnlyTranslocated"));
          setPhase("error");
          return;
        }
        if (location === "translocated_healed") {
          setError(t("update.readOnlyTranslocatedHealed"));
          setPhase("error");
          return;
        }

        setPhase("checking");
        const update = await check();

        if (cancelled) {
          return;
        }
        // Nothing newer after all: there is nothing to wait for here.
        if (!update) {
          onContinue();
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
        // The plugin doesn't relaunch.
        await relaunch();
      } catch (err) {
        if (!cancelled) {
          console.error("[update] Auto-update failed:", err);
          const message = errorMessage(err);
          setError(isReadOnlyError(message) ? t("update.readOnlyGeneric") : message);
          setPhase("error");
        }
      }
    }

    runUpdate();
    return () => { cancelled = true; };
    // onContinue is read once per attempt; a new identity must not restart a running install.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [t, attempt]);

  const retry = () => {
    setError(null);
    setProgress(null);
    setPhase("preparing");
    setAttempt((n) => n + 1);
  };

  const openDownload = () => {
    shellOpen(DOWNLOAD_URL).catch((e) => console.warn("[update] open failed:", e));
  };

  const label = (() => {
    switch (phase) {
      case "preparing":
        return t("update.preparing");
      case "checking":
        return t("update.checking");
      case "downloading":
        return progress !== null
          ? t("update.downloadingProgress", { progress })
          : t("update.downloading");
      case "installing":
        return t("update.installing");
      case "relaunching":
        return t("update.relaunching");
      case "error":
        return t("update.failed", { error });
    }
  })();

  return (
    <div
      data-testid="update-screen"
      className="bg-bg"
      style={{
        height: "100%",
        width: "100%",
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
            data-testid="update-status"
            role={phase === "error" ? "alert" : undefined}
            className={`text-xs font-mono break-words ${phase === "error" ? "text-danger" : "text-fg"}`}
          >
            {label}
          </span>
        </div>

        {phase === "error" && (
          <div data-testid="update-failed-actions" className="flex flex-wrap justify-center gap-2">
            <Button data-testid="update-retry" onClick={retry}>
              {t("update.tryAgain")}
            </Button>
            <Button data-testid="update-download" variant="ghost" onClick={openDownload}>
              {t("clientBlocked.download")}
            </Button>
            <Button data-testid="update-continue" variant="ghost" onClick={onContinue}>
              {t("update.continueWithout")}
            </Button>
          </div>
        )}

        {phase === "downloading" && progress !== null && (
          <div
            className="bg-line"
            style={{
              width: "100%",
              maxWidth: 280,
              height: 4,
              borderRadius: 2,
              overflow: "hidden",
            }}
          >
            <div
              className="bg-accent"
              style={{
                width: `${progress}%`,
                height: "100%",
                transition: "width 0.2s ease",
              }}
            />
          </div>
        )}
      </div>
    </div>
  );
};
