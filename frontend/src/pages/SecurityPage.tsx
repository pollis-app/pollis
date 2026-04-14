import React, { useEffect, useState } from "react";
import { PageShell } from "../components/Layout/PageShell";
import { useAppStore } from "../stores/appStore";
import * as api from "../services/api";

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

function formatTimestamp(iso: string): string {
  try {
    const d = new Date(iso);
    return d.toLocaleString();
  } catch {
    return iso;
  }
}

export const SecurityPage: React.FC = () => {
  const { currentUser } = useAppStore();
  const [events, setEvents] = useState<api.SecurityEvent[] | null>(null);
  const [error, setError] = useState<string | null>(null);

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
    return () => {
      cancelled = true;
    };
  }, [currentUser?.id]);

  return (
    <PageShell title="Security" scrollable>
      <div
        className="flex flex-col gap-4 p-4 font-mono"
        data-testid="security-page"
        style={{ color: "var(--c-text)" }}
      >
        <div>
          <h2 className="text-sm font-bold" style={{ color: "var(--c-accent)" }}>
            Security events
          </h2>
          <p
            className="text-xs mt-1"
            style={{ color: "var(--c-text-muted)", lineHeight: 1.5 }}
          >
            Every time a device is added to your account, an enrollment is
            rejected, or your identity is reset, it shows up here. Check it
            if you ever suspect someone has accessed your account.
          </p>
        </div>

        {error && (
          <p
            data-testid="security-events-error"
            className="text-xs"
            style={{ color: "#ff6b6b" }}
          >
            {error}
          </p>
        )}

        {events === null && (
          <p className="text-xs" style={{ color: "var(--c-text-muted)" }}>
            Loading…
          </p>
        )}

        {events !== null && events.length === 0 && (
          <p
            data-testid="security-events-empty"
            className="text-xs"
            style={{ color: "var(--c-text-muted)" }}
          >
            No security events recorded yet.
          </p>
        )}

        {events !== null && events.length > 0 && (
          <ul className="flex flex-col gap-3" data-testid="security-events-list">
            {events.map((event) => {
              const { heading, detail } = describe(event);
              return (
                <li
                  key={event.id}
                  data-testid={`security-event-${event.id}`}
                  style={{
                    background: "var(--c-surface)",
                    border: "2px solid var(--c-border)",
                    borderRadius: "0.5rem",
                    padding: "0.75rem",
                  }}
                >
                  <div
                    className="text-xs font-bold"
                    style={{ color: "var(--c-text)" }}
                  >
                    {heading}
                  </div>
                  <div
                    className="text-xs mt-1"
                    style={{ color: "var(--c-text-muted)" }}
                  >
                    {detail}
                  </div>
                  <div
                    className="text-xs mt-2"
                    style={{ color: "var(--c-text-dim)" }}
                  >
                    {formatTimestamp(event.created_at)}
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </PageShell>
  );
};
