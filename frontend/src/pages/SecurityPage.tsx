import { errorMessage } from "../utils/errorMessage";
import React, { useEffect, useState, useCallback } from "react";
import { Trans, useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { useNavigate, useRouter } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { Button } from "../components/ui/Button";
import { TextInput } from "../components/ui/TextInput";
import { Switch } from "../components/ui/Switch";
import { NavigableList } from "../components/ui/NavigableList";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import type { RouterContext } from "../types/router";
import * as api from "../services/api";
import { AccountKeyAuditLine } from "../components/Security/AccountKeyAuditLine";
import { BuildVerifyLine } from "../components/Security/BuildVerifyLine";
import { ExportArchiveButton } from "../components/Security/ExportArchiveButton";
import { SecurityEventRow } from "../components/Security/SecurityEventRow";
import { shortId } from "../components/Security/securityEvents";
import { useSelfAuditAccountKey, useVerifyOwnBuild } from "../hooks/queries";
import {
  securityEventQueryKeys,
  useSecurityEventsPreview,
} from "../hooks/queries/useSecurityEvents";
import { useQueryClient } from "@tanstack/react-query";
import { getVersion, shellOpen } from "../bridge";
import { usePreferences } from "../hooks/queries/usePreferences";
import {
  useMediaPermissions,
  useRevokeMediaPermissions,
  type PermissionState,
} from "../hooks/queries/useMediaPermissions";
import { invoke } from "../bridge";
import { isMac, isLinux, isWindows } from "../utils/platform";
import { formatDateTime } from "../utils/format";
import { useShortcutLabel } from "../keyboard";
import {
  AUTO_LOCK_OPTIONS,
  loadDeviceAutoLockMinutes,
  saveDeviceAutoLockMinutes,
} from "../utils/autoLock";

// Explainer for this page (epic #589, topic 12).
const LEARN_DASHBOARDS_URL = "https://pollis.com/learn#reading-the-dashboards";

function permissionPill(
  t: TFunction<"settings">,
  state: PermissionState | undefined,
): {
  label: string;
  color: string;
} {
  switch (state) {
    case "granted":
      return { label: t("security.permissionGranted"), color: "var(--c-accent)" };
    case "denied":
      return { label: t("security.permissionDenied"), color: "var(--c-danger)" };
    case "notDetermined":
      return { label: t("security.permissionNotSet"), color: "var(--c-text-muted)" };
    case "perSession":
      return { label: t("security.permissionPerSession"), color: "var(--c-text-dim)" };
    case "unsupported":
      return { label: t("security.permissionNotApplicable"), color: "var(--c-text-muted)" };
    default:
      return { label: t("security.permissionChecking"), color: "var(--c-text-muted)" };
  }
}

const sectionHeaderClass =
  "text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line";

// Not translated: the label interpolates it, so every locale types the word the check accepts.
const DELETE_CONFIRM_WORD = "DELETE";

export const SecurityPage: React.FC = observer(() => {
  const { t } = useTranslation("settings");
  const navigate = useNavigate();
  const router = useRouter();
  const { onDeleteAccount } = router.options.context as RouterContext;
  const { currentUser } = appStore;
  const { data: selfAudit } = useSelfAuditAccountKey();
  // "This build" verification is on-demand (a mutation), never run on mount.
  const buildVerify = useVerifyOwnBuild();
  const [appVersion, setAppVersion] = useState<string | null>(null);
  // Only the newest few (#1548); the whole log is its own page.
  const queryClient = useQueryClient();
  const {
    data: events,
    isError: eventsFailed,
    error: eventsError,
  } = useSecurityEventsPreview();

  const [deleteConfirmText, setDeleteConfirmText] = useState("");
  const [isDeleting, setIsDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  const [devices, setDevices] = useState<api.DeviceInfo[] | null>(null);
  const [devicesError, setDevicesError] = useState<string | null>(null);
  const [confirmingDevice, setConfirmingDevice] = useState<api.DeviceInfo | null>(null);
  const [confirmInput, setConfirmInput] = useState("");
  const [revoking, setRevoking] = useState(false);

  // OS media permissions live here, beside Devices.
  const { query: prefsQuery, save: savePrefs } = usePreferences();
  const mediaPermissions = useMediaPermissions();
  const revokeMedia = useRevokeMediaPermissions();
  const [revokeMediaOnExit, setRevokeMediaOnExit] = useState<boolean>(false);
  const [confirmingRevoke, setConfirmingRevoke] = useState<boolean>(false);

  // The in-app terminal (#282) is off by default; Rust's `device-settings.json` is the authority.
  const [terminalEnabled, setTerminalEnabled] = useState<boolean>(false);

  // Auto-lock (#851) is device-local, not synced.
  const [autoLockMinutes, setAutoLockMinutes] = useState<number | null>(null);
  const lockLabel = useShortcutLabel("app.lock");

  useEffect(() => {
    setAutoLockMinutes(loadDeviceAutoLockMinutes(currentUser?.id));
  }, [currentUser?.id]);

  useEffect(() => {
    void invoke<boolean>("get_terminal_enabled")
      // Anything but literal `true` means off.
      .then((enabled) => setTerminalEnabled(enabled === true))
      .catch(() => setTerminalEnabled(false));
  }, []);

  // Translated here: the options are module-level data.
  const autoLockLabel = (minutes: number | null): string => {
    if (minutes === null) {
      return t("security.autoLockOff");
    }
    if (minutes % 60 === 0) {
      return t("security.autoLockHours", { count: minutes / 60 });
    }
    return t("security.autoLockMinutes", { count: minutes });
  };

  const handleAutoLock = (minutes: number | null) => {
    setAutoLockMinutes(minutes);
    saveDeviceAutoLockMinutes(currentUser?.id, minutes);
    // Push now; the backend owns the deadline.
    void api.setAutoLockTimeout(minutes).catch((err) => {
      console.warn("[autolock] failed to apply timeout:", err);
    });
  };

  useEffect(() => {
    if (prefsQuery.data?.revoke_media_on_exit !== undefined) {
      setRevokeMediaOnExit(prefsQuery.data.revoke_media_on_exit);
    }
  }, [prefsQuery.data, currentUser?.id]);

  // Local only; nothing hits the log until the user clicks.
  useEffect(() => {
    let cancelled = false;
    getVersion()
      .then((v) => {
        if (!cancelled) {
          setAppVersion(v);
        }
      })
      .catch(() => {
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const handleTerminalEnabled = (val: boolean) => {
    setTerminalEnabled(val);
    void invoke("set_terminal_enabled", { enabled: val }).catch((err) => {
      console.warn("[terminal] set_terminal_enabled failed:", err);
      // Snap back: the file is the truth.
      setTerminalEnabled(!val);
    });
  };

  const handleRevokeMediaOnExit = (val: boolean) => {
    setRevokeMediaOnExit(val);
    // save replaces the whole blob, so merge.
    savePrefs({ ...(prefsQuery.data ?? {}), revoke_media_on_exit: val });
    // Push now so a quit right after picks it up.
    void invoke("set_revoke_media_on_exit", { enabled: val }).catch((err) => {
      console.warn("[media-permissions] set_revoke_media_on_exit failed:", err);
    });
  };

  const handleRevokeNow = () => {
    setConfirmingRevoke(false);
    revokeMedia.mutate(["camera", "microphone", "screen"]);
  };

  const loadDevices = React.useCallback(() => {
    if (!currentUser) {
      return;
    }
    api
      .listUserDevices(currentUser.id)
      .then(setDevices)
      .catch((err) => {
        setDevicesError(errorMessage(err, t("security.devicesLoadFailed")));
        setDevices([]);
      });
  }, [currentUser?.id, t]);

  useEffect(() => {
    if (!currentUser) {
      return;
    }
    loadDevices();
  }, [currentUser?.id, loadDevices]);

  const cancelConfirm = () => {
    setConfirmingDevice(null);
    setConfirmInput("");
  };

  const revoke = async () => {
    if (!currentUser || !confirmingDevice) {
      return;
    }
    setRevoking(true);
    setDevicesError(null);
    try {
      await api.revokeDevice(currentUser.id, confirmingDevice.device_id);
      cancelConfirm();
      loadDevices();
      // The revoke wrote a `device_revoked` event; show it.
      void queryClient.invalidateQueries({ queryKey: securityEventQueryKeys.all });
    } catch (err) {
      setDevicesError(errorMessage(err, t("security.revokeFailed")));
    } finally {
      setRevoking(false);
    }
  };

  const deviceDisplayName = (device: api.DeviceInfo): string =>
    device.device_name ?? shortId(device.device_id);

  const handleDeleteAccount = useCallback(async () => {
    if (!currentUser) {
      return;
    }
    if (deleteConfirmText !== DELETE_CONFIRM_WORD) {
      return;
    }
    setIsDeleting(true);
    setDeleteError(null);
    try {
      await api.deleteAccount(currentUser.id);
      // Clear local state even if the router callback chain is broken.
      appStore.logout();
      if (onDeleteAccount) {
        onDeleteAccount();
      } else {
        console.error("[SecurityPage] onDeleteAccount callback is undefined — falling back to logout only");
      }
    } catch (err) {
      setDeleteError(errorMessage(err, t("security.deleteAccountFailed")));
      setIsDeleting(false);
    }
  }, [currentUser, deleteConfirmText, onDeleteAccount, t]);

  return (
    <PageShell title={t("security.title")} scrollable>
      <div className="flex justify-center px-6 py-8">
        <div
          className="flex flex-col gap-8 w-full max-w-md font-mono"
          data-testid="security-page"
        >
          {/* Account key — advisory self-audit of your published identity key
              against the public transparency log (#330). */}
          <section className="flex flex-col gap-4 mb-12" data-testid="account-key-section">
            <h2 className={sectionHeaderClass}>
              {t("security.accountKeyHeading")}
            </h2>
            <p className="text-xs text-muted" style={{ lineHeight: 1.5 }}>
              {t("security.accountKeyDescription")}
            </p>
            {selfAudit && (
              <AccountKeyAuditLine
                status={selfAudit.status}
                detail={selfAudit.detail}
                testId="self-account-key-audit"
              />
            )}
          </section>

          {/* This build — optional, on-demand check that this running build's
              fingerprint is published in the public binaries transparency log
              (#484). Never mandatory, never gates launch/update. */}
          <section className="flex flex-col gap-4 mb-12" data-testid="this-build-section">
            <h2 className={sectionHeaderClass}>
              {t("security.thisBuildHeading")}
            </h2>
            <p className="text-xs text-muted" style={{ lineHeight: 1.5 }}>
              {t("security.thisBuildDescription")}
            </p>

            {/* Reciprocal link into the /learn explainer that decodes this page
                (epic #589, topic 12). The four verdicts below are easy to
                misread — "unavailable" is not an accusation — so the legend has
                to be reachable from the surface itself. */}
            <button
              type="button"
              data-testid="build-verify-learn-link"
              className="text-2xs font-mono underline self-start text-muted"
              onClick={() => {
                void shellOpen(LEARN_DASHBOARDS_URL);
              }}
            >
              {t("security.verdictsLink")}
            </button>

            {/* Version + commit of the running build. Commit is only shown once
                the check has run (it's baked into the report), and only if this
                build actually baked one in. */}
            <div className="flex flex-col gap-0.5 text-xs text-dim">
              <span data-testid="build-version">
                {t("security.buildVersion", {
                  version: buildVerify.data?.version ?? appVersion ?? "—",
                })}
              </span>
              {buildVerify.data?.commit && (
                <span data-testid="build-commit">
                  {t("security.buildCommit", { commit: shortId(buildVerify.data.commit) })}
                </span>
              )}
            </div>

            {buildVerify.data && (
              <BuildVerifyLine
                status={buildVerify.data.status}
                detail={buildVerify.data.detail}
                testId="own-build-verify"
              />
            )}

            {buildVerify.isError && (
              <p
                data-testid="build-verify-error"
                className="text-xs text-danger"
              >
                {t("security.buildVerifyError")}
              </p>
            )}

            <div className="self-start">
              <Button
                data-testid="verify-build-button"
                variant="secondary"
                isLoading={buildVerify.isPending}
                loadingText={t("security.verifying")}
                onClick={() => buildVerify.mutate()}
              >
                {t("security.verifyBuildButton")}
              </Button>
            </div>
          </section>

          {/* PIN */}
          <section className="flex flex-col gap-4 mb-12">
            <h2 className={sectionHeaderClass}>
              {t("security.pinHeading")}
            </h2>
            <p className="text-xs text-muted" style={{ lineHeight: 1.5 }}>
              {t("security.pinDescription")}
            </p>
            <div className="self-start">
              <Button
                data-testid="change-pin-button"
                activateOn="press"
                onClick={() => navigate({ to: "/security/change-pin" })}
              >
                {t("security.changePinButton")}
              </Button>
            </div>
          </section>

          {/* Auto-lock (#851) — sits with PIN because it is the same gate,
              just reached without pressing anything. Device-local: the answer
              depends on where this machine physically sits, so it deliberately
              does not sync to your other devices. */}
          <section className="flex flex-col gap-4 mb-12" data-testid="auto-lock-section">
            <h2 className={sectionHeaderClass}>
              {t("security.autoLockHeading")}
            </h2>
            <p className="text-xs text-muted" style={{ lineHeight: 1.5 }}>
              {t("security.autoLockDescription", { shortcut: lockLabel })}
            </p>
            {/* A group of toggle buttons rather than the `role="radiogroup"`
                the neighbouring selectors use: without `role="radio"` +
                `aria-checked` on the children (which would also commit us to
                arrow-key traversal) a radiogroup announces every option as
                unselected. `aria-pressed` states the selection honestly, and
                renders identically. */}
            <div
              role="group"
              aria-label={t("security.autoLockAriaLabel")}
              className="flex gap-2 flex-wrap"
            >
              {AUTO_LOCK_OPTIONS.map((option) => {
                const selected = autoLockMinutes === option.minutes;
                const label = autoLockLabel(option.minutes);
                return (
                  <Button
                    key={option.minutes ?? "off"}
                    variant={selected ? "primary" : "secondary"}
                    size="sm"
                    aria-label={label}
                    aria-pressed={selected}
                    data-testid={`auto-lock-${option.minutes ?? "off"}`}
                    onClick={() => {
                      if (selected) {
                        return;
                      }
                      handleAutoLock(option.minutes);
                    }}
                  >
                    {label}
                  </Button>
                );
              })}
            </div>
            <p className="text-xs text-muted" style={{ lineHeight: 1.5 }}>
              {t("security.autoLockNote")}
            </p>
          </section>

          {/* Link a new device by QR (#1207) */}
          {currentUser && (
            <section className="flex flex-col gap-4 mb-12">
              <h2 className={sectionHeaderClass}>{t("linkDevice.heading")}</h2>
              <p className="text-sm text-muted">{t("linkDevice.rowSub")}</p>
              <div>
                <Button data-testid="link-device-button" variant="secondary" activateOn="press" onClick={() => navigate({ to: "/security/link-device" })}>
                  {t("linkDevice.heading")}
                </Button>
              </div>
            </section>
          )}

          {/* Devices */}
          <section className="flex flex-col gap-4 mb-12">
            <h2 className={sectionHeaderClass}>
              {t("security.devicesHeading")}
            </h2>
            <p className="text-xs text-muted" style={{ lineHeight: 1.5 }}>
              {t("security.devicesDescription")}
            </p>

            {devicesError && (
              <p
                data-testid="devices-error"
                className="text-xs text-danger"
              >
                {devicesError}
              </p>
            )}

            {confirmingDevice ? (
              <div
                className="flex flex-col gap-3 bg-surface border-2 border-line"
                data-testid="revoke-confirm"
                style={{
                  borderRadius: "0.5rem",
                  padding: "0.75rem",
                }}
              >
                <p className="text-xs text-fg">
                  <Trans
                    t={t}
                    i18nKey="security.revokeConfirmPrompt"
                    values={{ device: deviceDisplayName(confirmingDevice) }}
                    components={{ name: <strong /> }}
                  />
                </p>
                <TextInput
                  label={t("security.revokeConfirmInputLabel", {
                    device: deviceDisplayName(confirmingDevice),
                  })}
                  value={confirmInput}
                  onChange={setConfirmInput}
                  autoFocus
                  data-testid="revoke-confirm-input"
                />
                <div className="flex gap-2">
                  <Button
                    data-testid="revoke-confirm-submit"
                    size="sm"
                    disabled={
                      confirmInput !== deviceDisplayName(confirmingDevice) || revoking
                    }
                    onClick={revoke}
                  >
                    {revoking ? t("security.revoking") : t("security.revokeConfirmSubmit")}
                  </Button>
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={revoking}
                    onClick={cancelConfirm}
                  >
                    {t("common:actions.cancel")}
                  </Button>
                </div>
              </div>
            ) : (
              <NavigableList<api.DeviceInfo>
                testId="devices-list"
                // Must not claim focus on mount or re-render (scroll jump).
                autoFocus={false}
                items={devices ?? []}
                isLoading={devices === null}
                loadingLabel={t("security.devicesLoading")}
                emptyLabel={t("security.devicesEmpty")}
                getKey={(d) => d.device_id}
                rowTestId={(d) => `device-${d.device_id}`}
                renderRow={(d) => (
                  <div className="min-w-0 flex flex-col">
                    <span className="truncate text-fg">
                      {deviceDisplayName(d)}
                    </span>
                    <span className="text-dim">
                      {t("security.deviceLastSeen", { time: formatDateTime(d.last_seen) })}
                    </span>
                  </div>
                )}
                controls={(d) =>
                  d.is_current
                    ? []
                    : [
                        <Button
                          key="revoke"
                          data-testid={`revoke-${d.device_id}`}
                          variant="secondary"
                          size="sm"
                          onClick={() => {
                            setConfirmingDevice(d);
                            setConfirmInput("");
                            setDevicesError(null);
                          }}
                        >
                          {t("security.revokeButton")}
                        </Button>,
                      ]
                }
              />
            )}
          </section>

          {/* Security events */}
          <section className="flex flex-col gap-4 mb-12">
            <h2 className={sectionHeaderClass}>
              {t("security.eventsHeading")}
            </h2>
            <p className="text-xs text-muted" style={{ lineHeight: 1.5 }}>
              {t("security.eventsDescription")}
            </p>

            {eventsFailed && (
              <p
                data-testid="security-events-error"
                className="text-xs text-danger"
              >
                {errorMessage(eventsError, t("security.eventsLoadFailed"))}
              </p>
            )}

            {/* Deliberately NOT a NavigableList. This is a read-only audit
                trail with nothing to select, and that component's container is
                `tabIndex={0}` + calls `.focus()` from an effect keyed on
                `items`/`getKey` — both fresh identities on every render — so any
                unrelated re-render of this page (a media-permissions refetch on
                window focus, a keystroke in the revoke-confirm field) yanked
                focus here and scrolled the user down to it. A plain list has no
                focus to steal. */}
            {events === undefined ? (
              !eventsFailed && (
                <p className="text-xs font-mono text-muted">{t("common:states.loading")}</p>
              )
            ) : events.length === 0 ? (
              <p className="text-xs font-mono text-dim">
                {t("security.eventsEmpty")}
              </p>
            ) : (
              <div data-testid="security-events-list" className="flex flex-col">
                {/* Only the newest few (#1548): the DS is asked for exactly
                    that many, and the full log is the Security events page. */}
                {events.map((event) => (
                  <SecurityEventRow key={event.id} event={event} />
                ))}
              </div>
            )}
            {events !== undefined && events.length > 0 && (
              <div className="self-start">
                <Button
                  data-testid="security-events-view-all"
                  variant="secondary"
                  size="sm"
                  onClick={() => navigate({ to: "/security/events" })}
                >
                  {t("security.eventsViewAll")}
                </Button>
              </div>
            )}
          </section>

          {/* Media permissions — OS camera/mic/screen access: live status,
              revoke-on-quit, and a manual revoke. An access-control concern,
              so it sits with Devices rather than in Preferences. */}
          <section className="flex flex-col gap-4 mb-12">
            <h2 className={sectionHeaderClass}>
              {t("security.mediaHeading")}
            </h2>

            {/* Live OS status for each media device. */}
            <div className="flex flex-col gap-2">
              {[
                {
                  key: "camera",
                  label: t("security.mediaCamera"),
                  state: mediaPermissions.data?.camera,
                },
                {
                  key: "microphone",
                  label: t("security.mediaMicrophone"),
                  state: mediaPermissions.data?.microphone,
                },
                {
                  key: "screen",
                  label: t("security.mediaScreenShare"),
                  state: mediaPermissions.data?.screen,
                },
              ].map((row) => {
                const pill = permissionPill(t, row.state);
                return (
                  <div key={row.key} className="flex items-center justify-between">
                    <span className="text-sm text-fg">
                      {row.label}
                    </span>
                    <span
                      className="text-xs font-mono px-2 py-0.5 rounded"
                      style={{ color: pill.color, border: `1px solid ${pill.color}` }}
                    >
                      {pill.label}
                    </span>
                  </div>
                );
              })}
            </div>

            <div className="flex flex-col gap-1.5">
              <Switch
                id="pref-revoke-media-on-exit"
                label={t("security.revokeMediaOnExitLabel")}
                checked={revokeMediaOnExit}
                onChange={handleRevokeMediaOnExit}
              />
              <p className="text-xs font-mono text-muted">
                {t("security.revokeMediaOnExitDescription")}
              </p>
            </div>

            {/* Inline confirm (NO modal) — clicking "Revoke now" swaps the
                button for a Confirm/Cancel row in place. */}
            <div className="self-start">
              {confirmingRevoke ? (
                <div className="flex items-center gap-2 flex-wrap">
                  <span className="text-xs font-mono text-dim">
                    {t("security.revokeNowNote")}
                  </span>
                  <Button variant="primary" size="sm" onClick={handleRevokeNow}>
                    {t("security.revokeNowConfirm")}
                  </Button>
                  <Button
                    variant="secondary"
                    size="sm"
                    onClick={() => setConfirmingRevoke(false)}
                  >
                    {t("common:actions.cancel")}
                  </Button>
                </div>
              ) : (
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={revokeMedia.isPending}
                  onClick={() => setConfirmingRevoke(true)}
                >
                  {t("security.revokeNowButton")}
                </Button>
              )}
            </div>

            {/* Result note from the last revoke, when the platform has one. */}
            {revokeMedia.data?.note && (
              <p className="text-xs font-mono text-muted">
                {revokeMedia.data.note}
              </p>
            )}

            {/* Honest, per-OS explanation of what "Revoke now" does. */}
            <p className="text-xs font-mono text-muted">
              {isMac && t("security.mediaNoteMac")}
              {isLinux && t("security.mediaNoteLinux")}
              {isWindows && t("security.mediaNoteWindows")}
              {!isMac && !isLinux && !isWindows && t("security.mediaNoteOther")}
            </p>
          </section>

          {/* The in-app terminal. A shell is the single most powerful thing
              this process can be asked to do, so it ships off and the switch
              lives here rather than in Preferences — it is a capability grant,
              not a comfort setting. */}
          <section className="flex flex-col gap-4 mb-12" data-testid="terminal-section">
            <h2 className={sectionHeaderClass}>
              {t("security.terminalHeading")}
            </h2>
            <div className="flex flex-col gap-1.5">
              <Switch
                id="pref-terminal-enabled"
                label={t("security.terminalEnabledLabel")}
                checked={terminalEnabled}
                onChange={handleTerminalEnabled}
              />
              <p className="text-xs font-mono text-muted">
                {t("security.terminalEnabledDescription")}
              </p>
            </div>
          </section>

          {/* Your data (#856) — the exit that is not delete_account. Strictly
              on-device: Rust reads the local DB and writes the file the user
              picked; nothing touches the network. Sits directly above the
              danger zone so "get my data out" is visibly next to "delete it". */}
          <section className="flex flex-col gap-4 mb-12" data-testid="export-section">
            <h2 className={sectionHeaderClass}>
              {t("security.exportHeading")}
            </h2>
            <p className="text-xs text-muted" style={{ lineHeight: 1.5 }}>
              {t("security.exportDescription")}
            </p>
            <p className="text-xs font-mono text-dim" style={{ lineHeight: 1.5 }}>
              {t("security.exportNote")}
            </p>
            <ExportArchiveButton
              fileStem={`pollis-archive-${new Date().toISOString().slice(0, 10)}`}
              testId="export-archive-button"
            />
          </section>

          {/* Danger zone — account deletion lives at the very bottom of the
              security page so it's the last thing a user can reach. */}
          <section className="flex flex-col gap-4 mb-12" data-testid="settings-danger-zone">
            <h2
              className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
              style={{ color: "hsl(0 60% 55%)", borderColor: "hsl(0 60% 30% / 40%)" }}
            >
              {t("security.dangerZoneHeading")}
            </h2>

            <p className="text-xs text-muted" style={{ lineHeight: 1.5 }}>
              {t("security.dangerZoneDescription")}
            </p>

            <TextInput
              label={t("security.deleteConfirmLabel", { word: DELETE_CONFIRM_WORD })}
              id="settings-delete-confirm"
              data-testid="settings-delete-confirm-input"
              value={deleteConfirmText}
              onChange={setDeleteConfirmText}
              placeholder={DELETE_CONFIRM_WORD}
              disabled={isDeleting}
              error={deleteError || undefined}
            />

            <Button
              data-testid="settings-delete-account-button"
              onClick={handleDeleteAccount}
              disabled={deleteConfirmText !== DELETE_CONFIRM_WORD || isDeleting}
              isLoading={isDeleting}
              loadingText={t("security.deletingAccount")}
              variant="danger"
              className="w-full"
            >
              {t("security.deleteAccountButton")}
            </Button>
          </section>
        </div>
      </div>
    </PageShell>
  );
});
