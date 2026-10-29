import type { TFunction } from "i18next";
import type { SecurityEvent } from "../../services/api";

/**
 * The Security events page's quick filters (#1548), as the `kinds` each one
 * sends to the DS. `all` sends none. Mobile carries the same table in
 * `mobile/lib/securityEvents.ts`; keep the two in step.
 */
export const SECURITY_EVENT_FILTERS: Record<
  "all" | "devices" | "identity" | "account",
  string[] | null
> = {
  all: null,
  devices: ["device_enrolled", "device_rejected", "device_revoked"],
  identity: [
    "identity_reset",
    "identity_rotated",
    "secret_key_rotated",
    "recovery_blob_fetched",
  ],
  account: ["email_changed"],
};

export type SecurityEventFilter = keyof typeof SECURITY_EVENT_FILTERS;

export const SECURITY_EVENT_FILTER_ORDER: SecurityEventFilter[] = [
  "all",
  "devices",
  "identity",
  "account",
];

/// The two addresses in an `email_changed` row's metadata, or `null` when the
/// row does not carry the expected JSON object. Never throws: the metadata
/// column is free-form and a row written by some other build must render as an
/// event with a vaguer detail line, not as a crashed page.
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

/// Human-readable summary for each `security_event.kind` the backend
/// currently emits. Unknown kinds fall through to the raw string so we
/// never silently drop new event types.
export function describeSecurityEvent(
  t: TFunction<"settings">,
  event: SecurityEvent,
): { heading: string; detail: string } {
  switch (event.kind) {
    case "device_enrolled":
      return {
        heading: t("settings:security.eventDeviceEnrolledHeading"),
        detail: event.device_id
          ? t("settings:security.eventDeviceEnrolledDetail", { device: shortId(event.device_id) })
          : t("settings:security.eventDeviceEnrolledDetailUnknown"),
      };
    case "device_rejected":
      return {
        heading: t("settings:security.eventDeviceRejectedHeading"),
        detail: event.device_id
          ? t("settings:security.eventDeviceRejectedDetail", { device: shortId(event.device_id) })
          : t("settings:security.eventDeviceRejectedDetailUnknown"),
      };
    case "device_revoked": {
      if (!event.device_id) {
        return {
          heading: t("settings:security.eventDeviceRevokedHeading"),
          detail: t("settings:security.eventDeviceRevokedDetailUnknown"),
        };
      }
      // `name=<device name>` when the revoked row carried one (#947). The
      // device id alone answers "was it revoked"; the name answers "which
      // one", which is the question someone who has just lost a laptop is
      // actually asking.
      const name = event.metadata?.startsWith("name=")
        ? event.metadata.slice("name=".length)
        : null;
      return {
        heading: t("settings:security.eventDeviceRevokedHeading"),
        detail: name
          ? t("settings:security.eventDeviceRevokedDetailNamed", {
              name,
              device: shortId(event.device_id),
            })
          : t("settings:security.eventDeviceRevokedDetail", {
              device: shortId(event.device_id),
            }),
      };
    }
    case "identity_reset":
      return {
        heading: t("settings:security.eventIdentityResetHeading"),
        detail: t("settings:security.eventIdentityResetDetail"),
      };
    case "identity_rotated":
      // Written by the Delivery Service inside the rotation transaction, not
      // by a client. `credential=session` is the pre-enrollment soft reset
      // (email code only — the DS also wiped memberships and other devices);
      // `credential=signature` is a rotation from an enrolled device.
      return {
        heading: t("settings:security.eventIdentityRotatedHeading"),
        detail: event.metadata?.includes("credential=session")
          ? t("settings:security.eventIdentityRotatedDetailSession")
          : t("settings:security.eventIdentityRotatedDetailSignature"),
      };
    case "secret_key_rotated":
      return {
        heading: t("settings:security.eventSecretKeyRotatedHeading"),
        detail: t("settings:security.eventSecretKeyRotatedDetail"),
      };
    case "email_changed": {
      // Written by the Delivery Service as part of the change (#1161), not by
      // a client — a client that simply omits the call cannot suppress it.
      // Metadata is `{"from":"…","to":"…"}`; both addresses matter, because
      // "from" is the mailbox that had to approve the move and "to" is where
      // account recovery now points.
      const addresses = parseEmailChangeMetadata(event.metadata);
      return {
        heading: t("settings:security.eventEmailChangedHeading"),
        detail: addresses
          ? t("settings:security.eventEmailChangedDetail", addresses)
          : t("settings:security.eventEmailChangedDetailUnknown"),
      };
    }
    case "recovery_blob_fetched":
      // DS-authored by `POST /v1/read/recovery-blob`: a device holding only an
      // email code downloaded the account key wrapped under the Secret Key —
      // the first step of Secret-Key recovery. Useless without that key, but
      // the account holder should know it happened.
      return {
        heading: t("settings:security.eventRecoveryBlobFetchedHeading"),
        detail: t("settings:security.eventRecoveryBlobFetchedDetail"),
      };
    default:
      // The raw backend `kind` for an event type this build doesn't know
      // about: a wire value, deliberately not translated.
      return {
        heading: event.kind,
        detail: event.metadata ?? "",
      };
  }
}

export function shortId(id: string): string {
  if (id.length <= 10) {
    return id;
  }
  return `${id.slice(0, 6)}…${id.slice(-4)}`;
}
