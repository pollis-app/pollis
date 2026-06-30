import React, { useEffect, useState, useCallback } from "react";
import { useNavigate, useRouter } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { Button } from "../components/ui/Button";
import { TextInput } from "../components/ui/TextInput";
import { NavigableList } from "../components/ui/NavigableList";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import type { RouterContext } from "../types/router";
import * as api from "../services/api";
import { AccountKeyAuditLine } from "../components/Security/AccountKeyAuditLine";
import { useSelfAuditAccountKey } from "../hooks/queries";
import { formatDateTime } from "../utils/format";

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
  const [events, setEvents] = useState<api.SecurityEvent[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const [deleteConfirmText, setDeleteConfirmText] = useState("");
  const [isDeleting, setIsDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  const [devices, setDevices] = useState<api.DeviceInfo[] | null>(null);
  const [devicesError, setDevicesError] = useState<string | null>(null);
  const [confirmingDevice, setConfirmingDevice] = useState<api.DeviceInfo | null>(null);
  const [confirmInput, setConfirmInput] = useState("");
  const [revoking, setRevoking] = useState(false);

  const loadDevices = React.useCallback(() => {
    if (!currentUser) {
      return;
    }
    api
      .listUserDevices(currentUser.id)
      .then(setDevices)
      .catch((err) => {
        setDevicesError(err instanceof Error ? err.message : "Failed to load devices");
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
          setError(err instanceof Error ? err.message : "Failed to load security events");
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
      setDevicesError(err instanceof Error ? err.message : "Failed to revoke device");
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
      setDeleteError(err instanceof Error ? err.message : "Failed to delete account");
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

            <NavigableList<api.SecurityEvent>
              testId="security-events-list"
              items={events ?? []}
              isLoading={events === null}
              loadingLabel="Loading…"
              emptyLabel="No security events recorded yet."
              getKey={(e) => e.id}
              rowTestId={(e) => `security-event-${e.id}`}
              renderRow={(event) => {
                const { heading, detail } = describe(event);
                return (
                  <div className="min-w-0 flex flex-col">
                    <span style={{ color: "var(--c-text)" }}>{heading}</span>
                    {detail && (
                      <span style={{ color: "var(--c-text-muted)" }}>{detail}</span>
                    )}
                    <span style={{ color: "var(--c-text-dim)" }}>
                      {formatDateTime(event.created_at)}
                    </span>
                  </div>
                );
              }}
            />
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
