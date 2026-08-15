import React from "react";
import { useTranslation } from "react-i18next";
import { ShieldCheck, Clock, AlertTriangle, ShieldQuestion } from "lucide-react";
import { shellOpen } from "../../bridge";
import type { BuildVerifyStatus } from "../../types";

interface BuildVerifyLineProps {
  status: BuildVerifyStatus;
  // One-line, human-readable explanation from the report. Surfaced as the reason
  // on `mismatch` and `unavailable`; ignored for the other (self-explanatory)
  // statuses.
  detail: string;
  testId?: string;
}

// The public, third-party verification story the honest caveat links out to: the
// in-app check proves inclusion + hash match, NOT that the payload reproduces
// from source. That step is the independent rebuilders'.
const VERIFY_DOCS_URL =
  "https://github.com/actuallydan/pollis/blob/main/docs/verify-transparency-log.md";

// Terse copy + tone per status, mirroring AccountKeyAuditLine. Quiet and
// advisory — these alert, they never block launch/update. `mismatch` is the only
// danger-toned case (solid danger color — NO neon/glow per repo rules); the rest
// sit in accent/muted text.
//
// `labelKey` is a `settings` translation key rather than the copy itself: this
// table is module-level, so a `t()` here would snapshot the language at import
// time. It is resolved in the component below, on every render.
const statusConfig: Record<
  BuildVerifyStatus,
  { icon: React.ReactElement; labelKey: string; color: string }
> = {
  verified: {
    icon: <ShieldCheck size={14} aria-hidden="true" />,
    labelKey: "security.buildStatusVerified",
    color: "var(--c-accent)",
  },
  pending: {
    icon: <Clock size={14} aria-hidden="true" />,
    labelKey: "security.buildStatusPending",
    color: "var(--c-text-muted)",
  },
  mismatch: {
    icon: <AlertTriangle size={14} aria-hidden="true" />,
    labelKey: "security.buildStatusMismatch",
    color: "var(--c-danger)",
  },
  unavailable: {
    icon: <ShieldQuestion size={14} aria-hidden="true" />,
    labelKey: "security.buildStatusUnavailable",
    color: "var(--c-text-muted)",
  },
};

// A small, advisory status line surfacing the result of an in-app "verify this
// build" check (issue #484). Same deliberately-quiet pattern as
// AccountKeyAuditLine. On `mismatch` and `unavailable` it renders the report's
// reason and a link out to the third-party verification guide — for the install
// shapes that can't reproduce their own payload hash in-app (macOS .app,
// Windows NSIS, deb/rpm) that guide IS the verification path, so it has to be
// reachable from the quiet status too, not just the alarm.
export const BuildVerifyLine: React.FC<BuildVerifyLineProps> = ({
  status,
  detail,
  testId = "build-verify",
}) => {
  const { t } = useTranslation("settings");
  const { icon, labelKey, color } = statusConfig[status];
  const showReason = status === "mismatch" || status === "unavailable";

  return (
    <div
      data-testid={testId}
      data-status={status}
      className="flex flex-col gap-1"
    >
      <span
        className="inline-flex items-center gap-1 text-2xs font-mono"
        style={{ color }}
      >
        {icon}
        {t(labelKey)}
      </span>
      {/* Append the report's reason and the verify-guide link so the user can
          act on it. */}
      {showReason && (
        <>
          {detail && (
            <span
              data-testid={`${testId}-reason`}
              className="text-2xs font-mono"
              style={{ color }}
            >
              {detail}
            </span>
          )}
          <button
            type="button"
            data-testid={`${testId}-learn-more`}
            className="text-2xs font-mono underline self-start"
            style={{ color }}
            onClick={() => {
              void shellOpen(VERIFY_DOCS_URL);
            }}
          >
            {t("security.buildVerifyGuideLink")}
          </button>
        </>
      )}
    </div>
  );
};
