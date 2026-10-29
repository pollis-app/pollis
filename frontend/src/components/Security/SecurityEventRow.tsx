import React from "react";
import { useTranslation } from "react-i18next";
import type { SecurityEvent } from "../../services/api";
import { formatDateTime } from "../../utils/format";
import { parseServerTimestamp } from "../../utils/newMember";
import { describeSecurityEvent } from "./securityEvents";

/** One read-only row of the security-event log — Security page and Security events page. */
export const SecurityEventRow: React.FC<{ event: SecurityEvent }> = ({ event }) => {
  const { t } = useTranslation("settings");
  const { heading, detail } = describeSecurityEvent(t, event);
  // `created_at` is SQLite `datetime('now')`: UTC with no zone. Read as UTC, or
  // the shown time is off by the viewer's offset.
  const ms = parseServerTimestamp(event.created_at);
  const createdAt = ms === null ? event.created_at : new Date(ms).toISOString();
  return (
    <div
      data-testid={`security-event-${event.id}`}
      data-kind={event.kind}
      className="flex min-w-0 flex-col px-4 py-2 text-xs font-mono"
    >
      <span className="text-fg">{heading}</span>
      {detail && <span className="text-muted">{detail}</span>}
      <span className="text-dim">{formatDateTime(createdAt)}</span>
    </div>
  );
};
