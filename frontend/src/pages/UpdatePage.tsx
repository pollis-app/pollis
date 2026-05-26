import React, { useState, useEffect, useCallback } from "react";
import {
  getVersion,
  check as checkForUpdate,
  invoke,
} from "../bridge";
import { PageShell } from "../components/Layout/PageShell";
import { Button } from "../components/ui/Button";
import { useAppStore } from "../stores/appStore";

type Status = "checking" | "available" | "none" | "error";

export const UpdatePage: React.FC = () => {
  const setUpdateRequired = useAppStore((s) => s.setUpdateRequired);
  const cachedAvailable = useAppStore((s) => s.availableUpdateVersion);
  const setAvailableUpdateVersion = useAppStore((s) => s.setAvailableUpdateVersion);

  const [appVersion, setAppVersion] = useState<string>("");
  // Seed from the cached poller result so the page lands on "available"
  // immediately when reached from the bottom-bar indicator — no flash of
  // "checking…" before the user can click Install.
  const [status, setStatus] = useState<Status>(cachedAvailable ? "available" : "checking");
  const [version, setVersion] = useState<string>(cachedAvailable ?? "");
  const [errorMessage, setErrorMessage] = useState<string>("");
  const [isInstalling, setIsInstalling] = useState(false);

  useEffect(() => {
    getVersion().then(setAppVersion).catch(() => setAppVersion("unknown"));
  }, []);

  // Always re-check on mount so the displayed version is fresh even when
  // the poller cache is stale. Does not block the seeded "available" UI.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const update = await checkForUpdate();
        if (cancelled) {
          return;
        }
        if (update) {
          setStatus("available");
          setVersion(update.version);
          setAvailableUpdateVersion(update.version);
        } else {
          setStatus("none");
          setVersion("");
          setAvailableUpdateVersion(null);
        }
      } catch (err) {
        if (cancelled) {
          return;
        }
        setStatus("error");
        setErrorMessage(err instanceof Error ? err.message : "Failed to check for updates");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [setAvailableUpdateVersion]);

  const handleInstall = useCallback(async () => {
    if (status !== "available") {
      return;
    }
    setIsInstalling(true);
    // Mark the Rust-side flag and flip the store so App.tsx takes over
    // with the fullscreen UpdateScreen. The screen handles graceful
    // voice/network teardown, download, install, and relaunch.
    await invoke("mark_update_required").catch(() => {});
    setUpdateRequired(true);
  }, [status, setUpdateRequired]);

  return (
    <PageShell title="Software Update" scrollable>
      <div
        className="flex-1 flex flex-col overflow-auto"
        style={{ background: "var(--c-bg)" }}
      >
        <div className="flex-1 flex justify-center overflow-auto px-6 py-8">
          <div className="w-full max-w-md flex flex-col gap-8">
            <section className="flex flex-col gap-4">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
                style={{ color: "var(--c-text-dim)", borderColor: "var(--c-border)" }}
              >
                Software Update
              </h2>

              <div className="flex flex-col gap-2">
                <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                  Current version: <span style={{ color: "var(--c-text)" }}>{appVersion || "Loading..."}</span>
                </p>

                {status === "checking" && (
                  <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                    Checking for updates…
                  </p>
                )}

                {status === "available" && (
                  <p className="text-xs font-mono" style={{ color: "var(--c-accent)" }}>
                    Update available: {version}
                  </p>
                )}

                {status === "none" && (
                  <p className="text-xs font-mono" style={{ color: "var(--c-accent-dim)" }}>
                    You're up to date!
                  </p>
                )}

                {status === "error" && (
                  <p className="text-xs font-mono" style={{ color: "var(--c-danger)" }}>
                    {errorMessage}
                  </p>
                )}
              </div>

              {status === "available" && (
                <Button
                  onClick={handleInstall}
                  disabled={isInstalling}
                  isLoading={isInstalling}
                  loadingText="Installing…"
                  variant="primary"
                >
                  Install update
                </Button>
              )}
            </section>
          </div>
        </div>
      </div>
    </PageShell>
  );
};
