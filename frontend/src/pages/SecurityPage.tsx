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
import { useSelfAuditAccountKey, useVerifyOwnBuild } from "../hooks/queries";
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

// The /learn explainer for this page's verdicts (#589, topic 12).
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

/// The two addresses in an `email_changed` row's metadata, or `null`. Never
/// throws: the column is free-form.
function parseEmailChangeMetadata(
  metadata: string | null | undefined,
): { from: string; to: string } | null {
  if (!metadata) {
    return null;
  }
  try {
    const parsed: unknown = JSON.parse(metadata);
    if (parsed && typeof parsed === "object") {
      const { from, to } = parsed as { from?: unknown; to?: unknown };
      if (typeof from === "string" && typeof to === "string") {
        return { from, to };
      }
    }
  } catch {
    // Not JSON. Fall through to the unknown-shape copy.
  }
  return null;
}

/// Summary per `security_event.kind`; unknown kinds show raw, never dropped.
function describe(
  t: TFunction<"settings">,
  event: api.SecurityEvent,
): { heading: string; detail: string } {
  switch (event.kind) {
    case "device_enrolled":
      return {
        heading: t("security.eventDeviceEnrolledHeading"),
        detail: event.device_id
          ? t("security.eventDeviceEnrolledDetail", { device: shortId(event.device_id) })
          : t("security.eventDeviceEnrolledDetailUnknown"),
      };
    case "device_rejected":
      return {
        heading: t("security.eventDeviceRejectedHeading"),
        detail: event.device_id
          ? t("security.eventDeviceRejectedDetail", { device: shortId(event.device_id) })
          : t("security.eventDeviceRejectedDetailUnknown"),
      };
    case "device_revoked": {
      if (!event.device_id) {
        return {
          heading: t("security.eventDeviceRevokedHeading"),
          detail: t("security.eventDeviceRevokedDetailUnknown"),
        };
      }
      // `name=<device name>` (#947): which device, not just whether.
      const name = event.metadata?.startsWith("name=")
        ? event.metadata.slice("name=".length)
        : null;
      return {
        heading: t("security.eventDeviceRevokedHeading"),
        detail: name
          ? t("security.eventDeviceRevokedDetailNamed", {
              name,
              device: shortId(event.device_id),
            })
          : t("security.eventDeviceRevokedDetail", {
              device: shortId(event.device_id),
            }),
      };
    }
    case "identity_reset":
      return {
        heading: t("security.eventIdentityResetHeading"),
        detail: t("security.eventIdentityResetDetail"),
      };
    case "identity_rotated":
      // DS-written in the rotation transaction. `credential=session`: the
      // pre-enrollment soft reset; `credential=signature`: an enrolled device.
      return {
        heading: t("security.eventIdentityRotatedHeading"),
        detail: event.metadata?.includes("credential=session")
          ? t("security.eventIdentityRotatedDetailSession")
          : t("security.eventIdentityRotatedDetailSignature"),
      };
    case "secret_key_rotated":
      return {
        heading: t("security.eventSecretKeyRotatedHeading"),
        detail: t("security.eventSecretKeyRotatedDetail"),
      };
    case "email_changed": {
      // DS-written (#1161), so a client can't suppress it. `{"from","to"}`: the
      // approving mailbox and the new recovery address.
      const addresses = parseEmailChangeMetadata(event.metadata);
      return {
        heading: t("security.eventEmailChangedHeading"),
        detail: addresses
          ? t("security.eventEmailChangedDetail", addresses)
          : t("security.eventEmailChangedDetailUnknown"),
      };
    }
    default:
      // Unknown kind: a wire value, not translated.
      return {
        heading: event.kind,
        detail: event.metadata ?? "",
      };
  }
}

function shortId(id: string): string {
  if (id.length <= 10) {
    return id;
  }
  return `${id.slice(0, 6)}…${id.slice(-4)}`;
}

// Display slice; the backend already caps at 100 newest-first.
const SECURITY_EVENTS_PAGE_SIZE = 20;

const sectionHeaderClass =
  "text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line";

// Typed to arm deletion. NOT translatable: label and placeholder interpolate
// it, so no locale can ask for a word the comparison rejects.
const DELETE_CONFIRM_WORD = "DELETE";

export const SecurityPage: React.FC = observer(() => {
  const { t } = useTranslation("settings");
  const navigate = useNavigate();
  const router = useRouter();
  const { onDeleteAccount } = router.options.context as RouterContext;
  const { currentUser } = appStore;
  const { data: selfAudit } = useSelfAuditAccountKey();
  const buildVerify = useVerifyOwnBuild();
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const [events, setEvents] = useState<api.SecurityEvent[] | null>(null);
  const [visibleEvents, setVisibleEvents] = useState(SECURITY_EVENTS_PAGE_SIZE);
  const [error, setError] = useState<string | null>(null);

  const [deleteConfirmText, setDeleteConfirmText] = useState("");
  const [isDeleting, setIsDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  const [devices, setDevices] = useState<api.DeviceInfo[] | null>(null);
  const [devicesError, setDevicesError] = useState<string | null>(null);
  const [confirmingDevice, setConfirmingDevice] = useState<api.DeviceInfo | null>(null);
  const [confirmInput, setConfirmInput] = useState("");
  const [revoking, setRevoking] = useState(false);

  // OS media permissions: an access-control concern, so here, not Preferences.
  const { query: prefsQuery, save: savePrefs } = usePreferences();
  const mediaPermissions = useMediaPermissions();
  const revokeMedia = useRevokeMediaPermissions();
  const [revokeMediaOnExit, setRevokeMediaOnExit] = useState<boolean>(false);
  const [confirmingRevoke, setConfirmingRevoke] = useState<boolean>(false);

  // The in-app terminal (#282) ships off. Rust's `device-settings.json` is the
  // authority (`terminal_open` refuses regardless); this is the switch, not the lock.
  const [terminalEnabled, setTerminalEnabled] = useState<boolean>(false);

  // Idle auto-lock (#851): device-local, never synced.
  const [autoLockMinutes, setAutoLockMinutes] = useState<number | null>(null);
  const lockLabel = useShortcutLabel("app.lock");

  useEffect(() => {
    setAutoLockMinutes(loadDeviceAutoLockMinutes(currentUser?.id));
  }, [currentUser?.id]);

  useEffect(() => {
    void invoke<boolean>("get_terminal_enabled")
        // Anything but literal `true` (e.g. an older build's null) means off.
      .then((enabled) => setTerminalEnabled(enabled === true))
      .catch(() => setTerminalEnabled(false));
  }, []);

  // Labels translated here, keyed on `minutes` (module data can't translate).
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
    // Push now: the backend owns the deadline.
    void api.setAutoLockTimeout(minutes).catch((err) => {
      console.warn("[autolock] failed to apply timeout:", err);
    });
  };

  useEffect(() => {
    if (prefsQuery.data?.revoke_media_on_exit !== undefined) {
      setRevokeMediaOnExit(prefsQuery.data.revoke_media_on_exit);
    }
  }, [prefsQuery.data, currentUser?.id]);

  // Local version; no transparency-log call until the user clicks.
  useEffect(() => {
    let cancelled = false;
    getVersion()
      .then((v) => {
        if (!cancelled) {
          setAppVersion(v);
        }
      })
      .catch(() => {
        // Non-fatal — the section still renders and the verify button works.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const handleTerminalEnabled = (val: boolean) => {
    setTerminalEnabled(val);
    void invoke("set_terminal_enabled", { enabled: val }).catch((err) => {
      console.warn("[terminal] set_terminal_enabled failed:", err);
      setTerminalEnabled(!val);
    });
  };

  const handleRevokeMediaOnExit = (val: boolean) => {
    setRevokeMediaOnExit(val);
    // Merge: save replaces the whole blob.
    savePrefs({ ...(prefsQuery.data ?? {}), revoke_media_on_exit: val });
    // Push now so an immediate quit sees it.
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
    let cancelled = false;
    api
      .listSecurityEvents(currentUser.id)
      .then((rows) => {
        if (!cancelled) {
          setEvents(rows);
        }
      })
      .catch((err) => {
        if (!cancelled) {
          setError(errorMessage(err, t("security.eventsLoadFailed")));
          setEvents([]);
        }
      });
    loadDevices();
    return () => {
      cancelled = true;
    };
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
          {/* Advisory self-audit of your identity key against the log (#330). */}
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

          {/* Optional, on-demand build check against the binaries log (#484); never gates. */}
          <section className="flex flex-col gap-4 mb-12" data-testid="this-build-section">
            <h2 className={sectionHeaderClass}>
              {t("security.thisBuildHeading")}
            </h2>
            <p className="text-xs text-muted" style={{ lineHeight: 1.5 }}>
              {t("security.thisBuildDescription")}
            </p>

            {/* The /learn legend, reachable from here: "unavailable" is not an accusation. */}
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

            {/* Commit shows only after the check, and only if the build baked one in. */}
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
                onClick={() => navigate({ to: "/security/change-pin" })}
              >
                {t("security.changePinButton")}
              </Button>
            </div>
          </section>

          {/* Auto-lock (#851) sits with PIN: the same gate. Device-local. */}
          <section className="flex flex-col gap-4 mb-12" data-testid="auto-lock-section">
            <h2 className={sectionHeaderClass}>
              {t("security.autoLockHeading")}
            </h2>
            <p className="text-xs text-muted" style={{ lineHeight: 1.5 }}>
              {t("security.autoLockDescription", { shortcut: lockLabel })}
            </p>
            {/* Toggle buttons with `aria-pressed`, not a radiogroup: without
                `role="radio"` children it announces every option unselected. */}
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

          {currentUser && (
            <section className="flex flex-col gap-4 mb-12">
              <h2 className={sectionHeaderClass}>{t("linkDevice.heading")}</h2>
              <p className="text-sm text-muted">{t("linkDevice.rowSub")}</p>
              <div>
                <Button data-testid="link-device-button" variant="secondary" onClick={() => navigate({ to: "/security/link-device" })}>
                  {t("linkDevice.heading")}
                </Button>
              </div>
            </section>
          )}

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
                // Keyboard nav for Revoke, but never focus on mount or re-render.
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

          <section className="flex flex-col gap-4 mb-12">
            <h2 className={sectionHeaderClass}>
              {t("security.eventsHeading")}
            </h2>
            <p className="text-xs text-muted" style={{ lineHeight: 1.5 }}>
              {t("security.eventsDescription")}
            </p>

            {error && (
              <p
                data-testid="security-events-error"
                className="text-xs text-danger"
              >
                {error}
              </p>
            )}

            {/* NOT a NavigableList: a read-only trail, and its focus effect would
                steal focus on every unrelated re-render. */}
            {events === null ? (
              <p className="text-xs font-mono text-muted">{t("common:states.loading")}</p>
            ) : events.length === 0 ? (
              <p className="text-xs font-mono text-dim">
                {t("security.eventsEmpty")}
              </p>
            ) : (
              <div data-testid="security-events-list" className="flex flex-col">
                {events.slice(0, visibleEvents).map((event) => {
                  const { heading, detail } = describe(t, event);
                  return (
                    <div
                      key={event.id}
                      data-testid={`security-event-${event.id}`}
                      className="flex min-w-0 flex-col px-4 py-2 text-xs font-mono"
                    >
                      <span className="text-fg">{heading}</span>
                      {detail && <span className="text-muted">{detail}</span>}
                      <span className="text-dim">
                        {formatDateTime(event.created_at)}
                      </span>
                    </div>
                  );
                })}
                {/* Paging is a pure slice of the capped, newest-first list. */}
                {events.length > visibleEvents && (
                  <div className="px-4 pt-2">
                    <Button
                      data-testid="security-events-show-more"
                      variant="ghost"
                      size="sm"
                      onClick={() =>
                        setVisibleEvents((n) => n + SECURITY_EVENTS_PAGE_SIZE)
                      }
                    >
                      {t("security.eventsShowOlder", {
                        count: events.length - visibleEvents,
                      })}
                    </Button>
                  </div>
                )}
              </div>
            )}
          </section>

          <section className="flex flex-col gap-4 mb-12">
            <h2 className={sectionHeaderClass}>
              {t("security.mediaHeading")}
            </h2>

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

            {/* Inline confirm (no modal): "Revoke now" becomes Confirm/Cancel. */}
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

            {revokeMedia.data?.note && (
              <p className="text-xs font-mono text-muted">
                {revokeMedia.data.note}
              </p>
            )}

            <p className="text-xs font-mono text-muted">
              {isMac && t("security.mediaNoteMac")}
              {isLinux && t("security.mediaNoteLinux")}
              {isWindows && t("security.mediaNoteWindows")}
              {!isMac && !isLinux && !isWindows && t("security.mediaNoteOther")}
            </p>
          </section>

          {/* The terminal ships off, and its switch lives here: a capability
              grant, not a comfort setting. */}
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

          {/* Your data (#856): on-device export, right above the danger zone. */}
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
