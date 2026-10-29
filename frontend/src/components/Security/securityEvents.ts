import type { TFunction } from "i18next";
import type { SecurityEvent } from "../../services/api";

/**
 * The Security events page's quick filters (#1548), as the `kinds` each sends
 * to the DS. Mirrored in `mobile/lib/securityEvents.ts` (a mobile test compares).
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

// The two addresses in an `email_changed` row's metadata, or `null`. Never
// throws: metadata is free-form, and another build's row must still render.
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
    // Not JSON: fall through to the unknown-shape copy.
  }
  return null;
}

/** Summary for each `security_event.kind`; unknown kinds show the raw string. */
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
      // `name=<device name>` when the revoked row carried one (#947).
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
      // DS-written. `credential=session` is the pre-enrollment soft reset
      // (email code only); `credential=signature` is from an enrolled device.
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
      // DS-written, so a client cannot suppress it (#1161).
      const addresses = parseEmailChangeMetadata(event.metadata);
      return {
        heading: t("settings:security.eventEmailChangedHeading"),
        detail: addresses
          ? t("settings:security.eventEmailChangedDetail", addresses)
          : t("settings:security.eventEmailChangedDetailUnknown"),
      };
    }
    case "recovery_blob_fetched":
      // DS-written by `POST /v1/read/recovery-blob`: the first step of
      // Secret-Key recovery from a device holding only an email code.
      return {
        heading: t("settings:security.eventRecoveryBlobFetchedHeading"),
        detail: t("settings:security.eventRecoveryBlobFetchedDetail"),
      };
    default:
      // A wire value, deliberately not translated.
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
