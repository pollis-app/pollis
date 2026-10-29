import type { TFunction } from "i18next";
import type { SecurityEvent } from "../../services/api";

/** Quick filters (#1548) as the `kinds` each sends; `all` sends none. Keep in step with `mobile/lib/securityEvents.ts`. */
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

/// The `email_changed` addresses, or null; never throws on foreign metadata.
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
    // Not JSON: unknown-shape copy.
  }
  return null;
}

/// Unknown kinds fall through to the raw string.
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
      // `name=<device name>` when present (#947).
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
      // DS-written; `credential=session` is the pre-enrollment soft reset, `signature` a rotation.
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
      // DS-written (#1161), so a client can't suppress it.
      const addresses = parseEmailChangeMetadata(event.metadata);
      return {
        heading: t("settings:security.eventEmailChangedHeading"),
        detail: addresses
          ? t("settings:security.eventEmailChangedDetail", addresses)
          : t("settings:security.eventEmailChangedDetailUnknown"),
      };
    }
    case "recovery_blob_fetched":
      // DS-written by `POST /v1/read/recovery-blob`: first step of Secret-Key recovery.
      return {
        heading: t("settings:security.eventRecoveryBlobFetchedHeading"),
        detail: t("settings:security.eventRecoveryBlobFetchedDetail"),
      };
    default:
      // A wire value; deliberately untranslated.
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
