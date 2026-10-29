import { errorMessage as toErrorMessage } from "../utils/errorMessage";
import React, { useState, useEffect, useCallback } from "react";
import { Trans, useTranslation } from "react-i18next";
import {
  getVersion,
  check as checkForUpdate,
  invoke,
} from "../bridge";
import { PageShell } from "../components/Layout/PageShell";
import { Button } from "../components/ui/Button";
import { CodeSnippet } from "../components/ui/CodeSnippet";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import type { ManagedInstallInfo } from "../types";

type Status = "checking" | "available" | "none" | "error" | "managed";

export const UpdatePage: React.FC = observer(() => {
  const { t } = useTranslation("settings");
  const setUpdateRequired = appStore.setUpdateRequired;
  const cachedAvailable = appStore.availableUpdateVersion;
  const setAvailableUpdateVersion = appStore.setAvailableUpdateVersion;

  const [appVersion, setAppVersion] = useState<string>("");
  const [status, setStatus] = useState<Status>(cachedAvailable ? "available" : "checking");
  const [version, setVersion] = useState<string>(cachedAvailable ?? "");
  const [errorMessage, setErrorMessage] = useState<string>("");
  const [isInstalling, setIsInstalling] = useState(false);
  const [managed, setManaged] = useState<ManagedInstallInfo | null>(null);

  useEffect(() => {
    getVersion().then(setAppVersion).catch(() => setAppVersion(t("update.versionUnknown")));
  }, [t]);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        // Managed installs (AUR / .deb / .rpm) first: they can't use the in-app
        // updater, so show the package-manager banner even with no update.
        const m = await invoke<ManagedInstallInfo | null>("detect_managed_install").catch(() => null);
        if (cancelled) {
          return;
        }
        if (m) {
          setManaged(m);
          setStatus("managed");
          // In the background, so "Update available: vX" still shows.
          checkForUpdate()
            .then((update) => {
              if (cancelled) {
                return;
              }
              if (update) {
                setVersion(update.version);
                setAvailableUpdateVersion(update.version);
              } else {
                setAvailableUpdateVersion(null);
              }
            })
            .catch((e) => console.warn("checkForUpdate failed", e));
          return;
        }

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
        setErrorMessage(toErrorMessage(err, t("update.checkFailed")));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [setAvailableUpdateVersion, t]);

  const handleInstall = useCallback(async () => {
    if (status !== "available") {
      return;
    }
    setIsInstalling(true);
    await invoke("mark_update_required").catch((e) => console.warn("mark_update_required failed", e));
    setUpdateRequired(true);
  }, [status, setUpdateRequired]);

  return (
    <PageShell title={t("update.title")} scrollable>
      <div
        data-testid="update-page"
        className="flex-1 flex flex-col overflow-auto bg-bg"
      >
        <div className="flex-1 flex justify-center overflow-auto px-6 py-8">
          <div className="w-full max-w-md flex flex-col gap-8">
            <section className="flex flex-col gap-4">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest text-dim pb-1 border-b border-line"
              >
                {t("update.heading")}
              </h2>

              <div className="flex flex-col gap-2">
                <p className="text-xs font-mono text-muted">
                  <Trans
                    t={t}
                    i18nKey="update.currentVersion"
                    values={{ version: appVersion || t("update.versionLoading") }}
                    components={{ val: <span className="text-fg" /> }}
                  />
                </p>

                {status === "checking" && (
                  <p className="text-xs font-mono text-muted">
                    {t("update.checking")}
                  </p>
                )}

                {status === "available" && (
                  <p className="text-xs font-mono text-accent">
                    {t("update.available", { version })}
                  </p>
                )}

                {status === "none" && (
                  <p className="text-xs font-mono text-accent-dim">
                    {t("update.upToDate")}
                  </p>
                )}

                {status === "error" && (
                  <p className="text-xs font-mono text-danger">
                    {errorMessage}
                  </p>
                )}

                {status === "managed" && managed && (
                  <>
                    {version && (
                      <p className="text-xs font-mono text-accent">
                        {t("update.available", { version })}
                      </p>
                    )}
                    <p
                      className="text-xs font-mono text-muted"
                      style={{ lineHeight: 1.5 }}
                    >
                      {t("update.managedNote", { manager: managed.display_name })}
                    </p>
                    {managed.update_command && (
                      <div className="mt-2">
                        <CodeSnippet
                          value={managed.update_command}
                          data-testid="update-page-managed-command"
                          copyTestId="update-page-managed-copy"
                        />
                      </div>
                    )}
                  </>
                )}
              </div>

              {status === "available" && (
                <Button
                  onClick={handleInstall}
                  disabled={isInstalling}
                  isLoading={isInstalling}
                  loadingText={t("update.installing")}
                  variant="primary"
                >
                  {t("update.installButton")}
                </Button>
              )}
            </section>
          </div>
        </div>
      </div>
    </PageShell>
  );
});
