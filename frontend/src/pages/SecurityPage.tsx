import { errorMessage } from "../utils/errorMessage";
import React, { useEffect, useState, useCallback } from "react";
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
import { useSelfAuditAccountKey, useVerifyOwnBuild } from "../hooks/queries";
import { getVersion } from "../bridge";
import { usePreferences } from "../hooks/queries/usePreferences";
import {
  useMediaPermissions,
  useRevokeMediaPermissions,
  type PermissionState,
} from "../hooks/queries/useMediaPermissions";
import { invoke } from "../bridge";
import { isMac, isLinux, isWindows } from "../utils/platform";
import { formatDateTime } from "../utils/format";

// Map a PermissionState onto a human label + solid token color for the status
// pill. No neon/glow — solid text colors only.
function permissionPill(state: PermissionState | undefined): {
  label: string;
  color: string;
} {
  switch (state) {
    case "granted":
      return { label: "Granted", color: "var(--c-accent)" };
    case "denied":
      return { label: "Denied", color: "var(--c-danger)" };
    case "notDetermined":
      return { label: "Not set", color: "var(--c-text-muted)" };
    case "perSession":
      return { label: "Per session", color: "var(--c-text-dim)" };
    case "unsupported":
      return { label: "Not applicable", color: "var(--c-text-muted)" };
    default:
      return { label: "Checking…", color: "var(--c-text-muted)" };
  }
}

/// Human-readable summary for each `security_event.kind` the backend
/// currently emits. Unknown kinds fall through to the raw string so we
/// never silently drop new event types.
function describe(event: api.SecurityEvent): { heading: string; detail: string } {
  switch (event.kind) {
    case "device_enrolled":
      return {
        heading: "New device enrolled",
        detail: event.device_id
          ? `Device ${shortId(event.device_id)} was added to your account.`
          : "A new device was added to your account.",
      };
    case "device_rejected":
      return {
        heading: "Enrollment rejected",
        detail: event.device_id
          ? `A request from device ${shortId(event.device_id)} was rejected.`
          : "A device enrollment request was rejected.",
      };
    case "identity_reset":
      return {
        heading: "Account identity reset",
        detail:
          "You reset your account. All previous devices and groups were orphaned.",
      };
    case "secret_key_rotated":
      return {
        heading: "Secret Key rotated",
        detail: "Your Secret Key was changed. The old one no longer works.",
      };
    default:
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

// How many security events to render before "Show older events". The backend
// caps the fetch at 100 newest-first, so this is a display slice, not a query.
const SECURITY_EVENTS_PAGE_SIZE = 20;

const sectionHeaderClass =
  "text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b";
const sectionHeaderStyle: React.CSSProperties = {
  color: "var(--c-text)",
  borderColor: "var(--c-border)",
};

export const SecurityPage: React.FC = observer(() => {
  const navigate = useNavigate();
  const router = useRouter();
  const { onDeleteAccount } = router.options.context as RouterContext;
  const { currentUser } = appStore;
  const { data: selfAudit } = useSelfAuditAccountKey();
  // "This build" verification is on-demand (a mutation), never run on mount.
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

  // OS media permissions (camera / mic / screen): live status + revoke-on-quit
  // pref + manual revoke. This is an access-control concern, so it lives here
  // next to Devices rather than in Preferences.
  const { query: prefsQuery, save: savePrefs } = usePreferences();
  const mediaPermissions = useMediaPermissions();
  const revokeMedia = useRevokeMediaPermissions();
  const [revokeMediaOnExit, setRevokeMediaOnExit] = useState<boolean>(false);
  const [confirmingRevoke, setConfirmingRevoke] = useState<boolean>(false);

  useEffect(() => {
    if (prefsQuery.data?.revoke_media_on_exit !== undefined) {
      setRevokeMediaOnExit(prefsQuery.data.revoke_media_on_exit);
    }
  }, [prefsQuery.data, currentUser?.id]);

  // The running app version, shown in the "This build" section. Cheap and
  // local — no transparency-log network call happens until the user clicks.
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

  const handleRevokeMediaOnExit = (val: boolean) => {
    setRevokeMediaOnExit(val);
    // Merge into the existing prefs blob (save replaces the whole blob), so we
    // never clobber unrelated preferences edited on the Preferences page.
    savePrefs({ ...(prefsQuery.data ?? {}), revoke_media_on_exit: val });
    // Push immediately so a quit right after toggling picks up the new value
    // without waiting for the throttled prefs round-trip.
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
        setDevicesError(errorMessage(err, "Failed to load devices"));
        setDevices([]);
      });
  }, [currentUser?.id]);

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
          setError(errorMessage(err, "Failed to load security events"));
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
      setDevicesError(errorMessage(err, "Failed to revoke device"));
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
    if (deleteConfirmText !== "DELETE") {
      return;
    }
    setIsDeleting(true);
    setDeleteError(null);
    try {
      await api.deleteAccount(currentUser.id);
      // Clear local state immediately so the user is logged out even if the
      // callback chain from the router context is broken.
      appStore.logout();
      if (onDeleteAccount) {
        onDeleteAccount();
      } else {
        console.error("[SecurityPage] onDeleteAccount callback is undefined — falling back to logout only");
      }
    } catch (err) {
      setDeleteError(errorMessage(err, "Failed to delete account"));
      setIsDeleting(false);
    }
  }, [currentUser, deleteConfirmText, onDeleteAccount]);

  return (
    <PageShell title="Security" scrollable>
      <div className="flex justify-center px-6 py-8">
        <div
          className="flex flex-col gap-8 w-full max-w-md font-mono"
          data-testid="security-page"
        >
          {/* Account key — advisory self-audit of your published identity key
              against the public transparency log (#330). */}
          <section className="flex flex-col gap-4 mb-12" data-testid="account-key-section">
            <h2 className={sectionHeaderClass} style={sectionHeaderStyle}>
              Account key
            </h2>
            <p className="text-xs" style={{ color: "var(--c-text-muted)", lineHeight: 1.5 }}>
              Your identity key is published to a public, append-only log so
              anyone can confirm contacts are talking to the real you. This
              checks that the log agrees with the key on this device.
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
            <h2 className={sectionHeaderClass} style={sectionHeaderStyle}>
              This build
            </h2>
            <p className="text-xs" style={{ color: "var(--c-text-muted)", lineHeight: 1.5 }}>
              Every release Pollis ships is fingerprinted into the same public,
              append-only log. This confirms the build you're running is one
              Pollis published there and independently verified by third-party
              rebuilders — it does not by itself prove the build matches the
              source, since a tampered app could lie about its own fingerprint.
            </p>

            {/* Version + commit of the running build. Commit is only shown once
                the check has run (it's baked into the report), and only if this
                build actually baked one in. */}
            <div className="flex flex-col gap-0.5 text-xs" style={{ color: "var(--c-text-dim)" }}>
              <span data-testid="build-version">
                Version {buildVerify.data?.version ?? appVersion ?? "—"}
              </span>
              {buildVerify.data?.commit && (
                <span data-testid="build-commit">
                  Commit {shortId(buildVerify.data.commit)}
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
                className="text-xs"
                style={{ color: "var(--c-danger)" }}
              >
                Couldn't check this build right now. Try again in a moment.
              </p>
            )}

            <div className="self-start">
              <Button
                data-testid="verify-build-button"
                variant="secondary"
                isLoading={buildVerify.isPending}
                loadingText="Verifying…"
                onClick={() => buildVerify.mutate()}
              >
                Verify this build
              </Button>
            </div>
          </section>

          {/* PIN */}
          <section className="flex flex-col gap-4 mb-12">
            <h2 className={sectionHeaderClass} style={sectionHeaderStyle}>
              PIN
            </h2>
            <p className="text-xs" style={{ color: "var(--c-text-muted)", lineHeight: 1.5 }}>
              The local PIN unlocks Pollis on this device. It never leaves the
              device and can't be recovered — use your Secret Key if you forget it.
            </p>
            <div className="self-start">
              <Button
                data-testid="change-pin-button"
                onClick={() => navigate({ to: "/security/change-pin" })}
              >
                Change PIN
              </Button>
            </div>
          </section>

          {/* Devices */}
          <section className="flex flex-col gap-4 mb-12">
            <h2 className={sectionHeaderClass} style={sectionHeaderStyle}>
              Devices
            </h2>
            <p className="text-xs" style={{ color: "var(--c-text-muted)", lineHeight: 1.5 }}>
              Every device signed in to your account. Revoke any you don't
              recognise — the device loses access to all groups and DMs on its
              next sync.
            </p>

            {devicesError && (
              <p
                data-testid="devices-error"
                className="text-xs"
                style={{ color: "var(--c-danger)" }}
              >
                {devicesError}
              </p>
            )}

            {confirmingDevice ? (
              <div
                className="flex flex-col gap-3"
                data-testid="revoke-confirm"
                style={{
                  background: "var(--c-surface)",
                  border: "2px solid var(--c-border)",
                  borderRadius: "0.5rem",
                  padding: "0.75rem",
                }}
              >
                <p className="text-xs" style={{ color: "var(--c-text)" }}>
                  Revoke <strong>{deviceDisplayName(confirmingDevice)}</strong>? This
                  cannot be undone.
                </p>
                <TextInput
                  label={`Type "${deviceDisplayName(confirmingDevice)}" to confirm`}
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
                    {revoking ? "Revoking…" : "Revoke device"}
                  </Button>
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={revoking}
                    onClick={cancelConfirm}
                  >
                    Cancel
                  </Button>
                </div>
              </div>
            ) : (
              <NavigableList<api.DeviceInfo>
                testId="devices-list"
                // Keeps its keyboard navigation (rows have Revoke buttons), but
                // must not claim focus on mount or on any re-render — same
                // scroll-jump the events list had, just further up the page.
                autoFocus={false}
                items={devices ?? []}
                isLoading={devices === null}
                loadingLabel="Loading devices…"
                emptyLabel="No devices registered."
                getKey={(d) => d.device_id}
                rowTestId={(d) => `device-${d.device_id}`}
                renderRow={(d) => (
                  <div className="min-w-0 flex flex-col">
                    <span className="truncate" style={{ color: "var(--c-text)" }}>
                      {deviceDisplayName(d)}
                    </span>
                    <span style={{ color: "var(--c-text-dim)" }}>
                      Last seen {formatDateTime(d.last_seen)}
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
                          Revoke
                        </Button>,
                      ]
                }
              />
            )}
          </section>

          {/* Security events */}
          <section className="flex flex-col gap-4 mb-12">
            <h2 className={sectionHeaderClass} style={sectionHeaderStyle}>
              Security events
            </h2>
            <p className="text-xs" style={{ color: "var(--c-text-muted)", lineHeight: 1.5 }}>
              Every time a device is added to your account, an enrollment is
              rejected, or your identity is reset, it shows up here. Check it if
              you ever suspect someone has accessed your account.
            </p>

            {error && (
              <p
                data-testid="security-events-error"
                className="text-xs"
                style={{ color: "var(--c-danger)" }}
              >
                {error}
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
            {events === null ? (
              <p className="text-xs font-mono text-muted">Loading…</p>
            ) : events.length === 0 ? (
              <p className="text-xs font-mono text-dim">
                No security events recorded yet.
              </p>
            ) : (
              <div data-testid="security-events-list" className="flex flex-col">
                {events.slice(0, visibleEvents).map((event) => {
                  const { heading, detail } = describe(event);
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
                {/* The backend already returns newest-first, capped at 100, so
                    paging is a pure slice — no refetch, no cursor. */}
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
                      Show older events ({events.length - visibleEvents} more)
                    </Button>
                  </div>
                )}
              </div>
            )}
          </section>

          {/* Media permissions — OS camera/mic/screen access: live status,
              revoke-on-quit, and a manual revoke. An access-control concern,
              so it sits with Devices rather than in Preferences. */}
          <section className="flex flex-col gap-4 mb-12">
            <h2 className={sectionHeaderClass} style={sectionHeaderStyle}>
              Media permissions
            </h2>

            {/* Live OS status for each media device. */}
            <div className="flex flex-col gap-2">
              {[
                { label: "Camera", state: mediaPermissions.data?.camera },
                { label: "Microphone", state: mediaPermissions.data?.microphone },
                { label: "Screen share", state: mediaPermissions.data?.screen },
              ].map((row) => {
                const pill = permissionPill(row.state);
                return (
                  <div key={row.label} className="flex items-center justify-between">
                    <span className="text-sm" style={{ color: "var(--c-text)" }}>
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
                label="Revoke system permissions when Pollis quits"
                checked={revokeMediaOnExit}
                onChange={handleRevokeMediaOnExit}
              />
              <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                When on, Pollis clears its saved camera / microphone / screen
                permissions as it quits, so the OS asks again next time.
              </p>
            </div>

            {/* Inline confirm (NO modal) — clicking "Revoke now" swaps the
                button for a Confirm/Cancel row in place. */}
            <div className="self-start">
              {confirmingRevoke ? (
                <div className="flex items-center gap-2 flex-wrap">
                  <span className="text-xs font-mono" style={{ color: "var(--c-text-dim)" }}>
                    This clears Pollis's saved permissions.
                  </span>
                  <Button variant="primary" size="sm" onClick={handleRevokeNow}>
                    Confirm
                  </Button>
                  <Button
                    variant="secondary"
                    size="sm"
                    onClick={() => setConfirmingRevoke(false)}
                  >
                    Cancel
                  </Button>
                </div>
              ) : (
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={revokeMedia.isPending}
                  onClick={() => setConfirmingRevoke(true)}
                >
                  Revoke now
                </Button>
              )}
            </div>

            {/* Result note from the last revoke, when the platform has one. */}
            {revokeMedia.data?.note && (
              <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                {revokeMedia.data.note}
              </p>
            )}

            {/* Honest, per-OS explanation of what "Revoke now" does. */}
            <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
              {isMac &&
                "Clears Pollis's saved permission; macOS will ask again next time you use each feature."}
              {isLinux &&
                "Linux grants media access per session — Pollis stores no standing grant, so there's nothing to revoke here."}
              {isWindows &&
                "Camera and microphone status comes from Windows privacy settings. “Revoke now” opens those settings so you can turn Pollis off; screen sharing isn't tracked there."}
              {!isMac && !isLinux && !isWindows &&
                "Media permission controls aren't available on this platform."}
            </p>
          </section>

          {/* Danger zone — account deletion lives at the very bottom of the
              security page so it's the last thing a user can reach. */}
          <section className="flex flex-col gap-4 mb-12" data-testid="settings-danger-zone">
            <h2
              className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
              style={{ color: "hsl(0 60% 55%)", borderColor: "hsl(0 60% 30% / 40%)" }}
            >
              Danger Zone
            </h2>

            <p className="text-xs" style={{ color: "var(--c-text-muted)", lineHeight: 1.5 }}>
              Permanently delete your account and all associated data. This cannot be undone.
            </p>

            <TextInput
              label="Type DELETE to confirm"
              id="settings-delete-confirm"
              data-testid="settings-delete-confirm-input"
              value={deleteConfirmText}
              onChange={setDeleteConfirmText}
              placeholder="DELETE"
              disabled={isDeleting}
              error={deleteError || undefined}
            />

            <Button
              data-testid="settings-delete-account-button"
              onClick={handleDeleteAccount}
              disabled={deleteConfirmText !== "DELETE" || isDeleting}
              isLoading={isDeleting}
              loadingText="Deleting account…"
              variant="danger"
              className="w-full"
            >
              Delete my account
            </Button>
          </section>
        </div>
      </div>
    </PageShell>
  );
});
