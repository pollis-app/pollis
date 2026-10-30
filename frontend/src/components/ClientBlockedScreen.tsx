import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { check, invoke, relaunch, shellOpen } from "../bridge";
import type { ClientPolicyStatus, ManagedInstallInfo } from "../types";
import { errorMessage } from "../utils/errorMessage";
import { Button } from "./ui/Button";
import { Card } from "./ui/Card";
import { CodeSnippet } from "./ui/CodeSnippet";
import { TitleBar } from "./Layout/TitleBar";

/** Where a build that cannot update itself gets the latest version. */
export const DOWNLOAD_URL = "https://pollis.com/#download";

// Mirrors `ReadOnlyLocation` in src-tauri/src/commands/install_kind.rs.
type ReadOnlyLocation = "disk_image" | "translocated" | "translocated_healed";

type Phase =
  | { kind: "idle" }
  | { kind: "updating"; progress: number | null }
  | { kind: "relaunching" }
  | { kind: "failed"; detail: string }
  | { kind: "checking" };

interface Props {
  status: ClientPolicyStatus;
  /** A fresh check said this build is allowed again. */
  onUnblocked: () => void;
}

/**
 * The whole window when this build is below the published minimum (#1611): what happened, and
 * the one thing to do. Update now runs the Tauri updater; a package-manager install gets its
 * command; a failed or impossible update falls back to the download page. No other navigation.
 */
export const ClientBlockedScreen: React.FC<Props> = ({ status, onUnblocked }) => {
  const { t } = useTranslation("nav");
  const [current, setCurrent] = useState<ClientPolicyStatus>(status);
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  // undefined while detecting; null for a self-updating install.
  const [managed, setManaged] = useState<ManagedInstallInfo | null | undefined>(undefined);

  useEffect(() => {
    setCurrent(status);
  }, [status]);

  useEffect(() => {
    let cancelled = false;
    invoke<ManagedInstallInfo | null>("detect_managed_install")
      .catch(() => null)
      .then((m) => {
        if (!cancelled) {
          setManaged(m ?? null);
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const busy = phase.kind === "updating" || phase.kind === "relaunching" || phase.kind === "checking";

  const runUpdate = async () => {
    setPhase({ kind: "updating", progress: null });
    try {
      // A read-only location is sure to fail; say how to fix it instead.
      const location = await invoke<ReadOnlyLocation | null>("detect_read_only_location").catch(() => null);
      if (location === "disk_image") {
        setPhase({ kind: "failed", detail: t("update.readOnlyDiskImage") });
        return;
      }
      if (location === "translocated") {
        setPhase({ kind: "failed", detail: t("update.readOnlyTranslocated") });
        return;
      }
      if (location === "translocated_healed") {
        setPhase({ kind: "failed", detail: t("update.readOnlyTranslocatedHealed") });
        return;
      }
      const update = await check();
      if (!update) {
        // The updater has nothing newer (or could not reach the manifest): the download page does.
        setPhase({ kind: "failed", detail: "" });
        return;
      }
      // Leave voice so LiveKit disconnects before the binary is overwritten.
      await invoke("leave_voice_channel").catch(() => {});
      let total = 0;
      let done = 0;
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? 0;
        } else if (event.event === "Progress") {
          done += event.data.chunkLength;
          if (total > 0) {
            setPhase({ kind: "updating", progress: Math.round((done / total) * 100) });
          }
        }
      });
      setPhase({ kind: "relaunching" });
      await relaunch();
    } catch (err) {
      console.error("[client-policy] update failed:", err);
      setPhase({ kind: "failed", detail: errorMessage(err) });
    }
  };

  const retry = async () => {
    setPhase({ kind: "checking" });
    try {
      const next = await invoke<ClientPolicyStatus>("check_client_policy", { force: true });
      if (!next.blocked) {
        onUnblocked();
        return;
      }
      setCurrent(next);
    } catch (err) {
      console.warn("[client-policy] re-check failed:", err);
    }
    setPhase({ kind: "idle" });
  };

  const openDownload = () => {
    shellOpen(DOWNLOAD_URL).catch((e) => console.warn("[client-policy] open failed:", e));
  };

  const versions = current.minimum
    ? t("clientBlocked.versions", { version: current.version ?? "?", minimum: current.minimum })
    : t("clientBlocked.installed", { version: current.version ?? "?" });

  const updatingLabel =
    phase.kind === "updating" && phase.progress !== null
      ? t("update.downloadingProgress", { progress: phase.progress })
      : phase.kind === "relaunching"
        ? t("update.relaunching")
        : t("clientBlocked.updating");

  const failed = phase.kind === "failed";
  // Self-updating unless a package manager owns the install.
  const canSelfUpdate = managed === null;

  return (
    <div data-testid="client-blocked-screen" className="flex flex-col h-full w-full bg-bg">
      <TitleBar />
      <div className="flex-1 flex justify-center overflow-y-auto p-4">
        <Card padding="lg" className="w-full max-w-md my-auto">
          <div className="flex flex-col gap-4">
            <h1 className="text-base font-mono font-bold text-fg">{t("clientBlocked.title")}</h1>
            <p data-testid="client-blocked-message" className="text-sm text-dim leading-relaxed">
              {current.message ?? t("clientBlocked.body")}
            </p>
            <p data-testid="client-blocked-versions" className="text-xs font-mono text-muted">
              {versions}
            </p>

            {managed && (
              <div className="flex flex-col gap-2">
                <p className="text-sm text-dim">
                  {t("clientBlocked.packageManager", { source: managed.display_name })}
                </p>
                {managed.update_command && (
                  <CodeSnippet value={managed.update_command} data-testid="client-blocked-command" />
                )}
              </div>
            )}

            {failed && (
              <div data-testid="client-blocked-update-failed" role="alert" className="flex flex-col gap-1">
                <p className="text-sm text-fg">{t("clientBlocked.updateFailed")}</p>
                {phase.detail && <p className="text-xs font-mono text-muted break-words">{phase.detail}</p>}
              </div>
            )}

            <div className="flex flex-wrap gap-2">
              {canSelfUpdate && !failed && (
                <Button
                  data-testid="client-blocked-update"
                  onClick={runUpdate}
                  disabled={busy}
                  isLoading={phase.kind === "updating" || phase.kind === "relaunching"}
                  loadingText={updatingLabel}
                >
                  {t("clientBlocked.updateNow")}
                </Button>
              )}
              {(failed || (managed && !managed.update_command)) && (
                <Button data-testid="client-blocked-download" onClick={openDownload} disabled={busy}>
                  {t("clientBlocked.download")}
                </Button>
              )}
              <Button
                data-testid="client-blocked-retry"
                variant="ghost"
                onClick={retry}
                disabled={busy}
                isLoading={phase.kind === "checking"}
                loadingText={t("clientBlocked.checking")}
              >
                {t("clientBlocked.retry")}
              </Button>
            </div>
          </div>
        </Card>
      </div>
    </div>
  );
};
