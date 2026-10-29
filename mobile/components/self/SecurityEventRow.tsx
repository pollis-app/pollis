import { View, Text } from "react-native";
import i18n from "../../i18n";
import type { SecurityEvent } from "../../lib/securityEvents";
import { formatRelative } from "../../lib/relativeTime";
import { semantic, type as ty, fonts, space } from "../../theme/tokens";

export function shortId(id: string): string {
  if (id.length <= 10) {
    return id;
  }
  return `${id.slice(0, 6)}…${id.slice(-4)}`;
}

// Never throws: the metadata column is free-form, and a row from another
// build must render with vaguer copy rather than crash the screen.
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

// Mirrors desktop's `describe()`. Unknown kinds fall through to the raw
// string so new event types are never silently dropped.
export function describeEvent(event: SecurityEvent): {
  heading: string;
  detail: string;
} {
  switch (event.kind) {
    case "device_enrolled":
      return {
        heading: i18n.t("settings:security.eventDeviceEnrolledHeading"),
        detail: event.device_id
          ? i18n.t("settings:security.eventDeviceEnrolledDetail", {
              device: shortId(event.device_id),
            })
          : i18n.t("settings:security.eventDeviceEnrolledDetailUnknown"),
      };
    case "device_rejected":
      return {
        heading: i18n.t("settings:security.eventDeviceRejectedHeading"),
        detail: event.device_id
          ? i18n.t("settings:security.eventDeviceRejectedDetail", {
              device: shortId(event.device_id),
            })
          : i18n.t("settings:security.eventDeviceRejectedDetailUnknown"),
      };
    case "device_revoked": {
      if (!event.device_id) {
        return {
          heading: i18n.t("settings:security.eventDeviceRevokedHeading"),
          detail: i18n.t("settings:security.eventDeviceRevokedDetailUnknown"),
        };
      }
      // `name=<device name>` (#947): the device row is deleted on revoke, so
      // this is the last place its name is readable.
      const name = event.metadata?.startsWith("name=")
        ? event.metadata.slice("name=".length)
        : null;
      return {
        heading: i18n.t("settings:security.eventDeviceRevokedHeading"),
        detail: name
          ? i18n.t("settings:security.eventDeviceRevokedDetailNamed", {
              name,
              device: shortId(event.device_id),
            })
          : i18n.t("settings:security.eventDeviceRevokedDetail", {
              device: shortId(event.device_id),
            }),
      };
    }
    case "identity_reset":
      return {
        heading: i18n.t("settings:security.eventIdentityResetHeading"),
        detail: i18n.t("settings:security.eventIdentityResetDetail"),
      };
    case "identity_rotated":
      // `credential=session` is the pre-enrollment soft reset (email code
      // only); `credential=signature` a rotation from an enrolled device.
      return {
        heading: i18n.t("settings:security.eventIdentityRotatedHeading"),
        detail: event.metadata?.includes("credential=session")
          ? i18n.t("settings:security.eventIdentityRotatedDetailSession")
          : i18n.t("settings:security.eventIdentityRotatedDetailSignature"),
      };
    case "secret_key_rotated":
      return {
        heading: i18n.t("settings:security.eventSecretKeyRotatedHeading"),
        detail: i18n.t("settings:security.eventSecretKeyRotatedDetail"),
      };
    case "email_changed": {
      // DS-authored as part of the change (#1161), so a client cannot suppress
      // it by omitting a call. Metadata is `{"from":"…","to":"…"}`.
      const addresses = parseEmailChangeMetadata(event.metadata);
      return {
        heading: i18n.t("settings:security.eventEmailChangedHeading"),
        detail: addresses
          ? i18n.t("settings:security.eventEmailChangedDetail", addresses)
          : i18n.t("settings:security.eventEmailChangedDetailUnknown"),
      };
    }
    case "recovery_blob_fetched":
      // A device holding only an email code downloaded the Secret-Key-wrapped
      // account key: the first step of Secret-Key recovery.
      return {
        heading: i18n.t("settings:security.eventRecoveryBlobFetchedHeading"),
        detail: i18n.t("settings:security.eventRecoveryBlobFetchedDetail"),
      };
    default:
      return {
        heading: event.kind,
        detail: event.metadata ?? "",
      };
  }
}

export function SecurityEventRow({ event }: { event: SecurityEvent }) {
  const { heading, detail } = describeEvent(event);
  const when = formatRelative(event.created_at);
  return (
    <View
      testID={`row-security-event-${event.id}`}
      accessible
      accessibilityLabel={[heading, when, detail].filter(Boolean).join(", ")}
      style={{
        paddingHorizontal: space.xxl,
        paddingVertical: space.md,
        gap: 2,
        minHeight: 52,
        justifyContent: "center",
      }}
    >
      <View style={{ flexDirection: "row", alignItems: "baseline", gap: space.sm }}>
        <Text
          style={{
            flex: 1,
            minWidth: 0,
            fontFamily: fonts.medium,
            fontSize: 16,
            color: semantic.text,
          }}
        >
          {heading}
        </Text>
        {/* Never shrinks or wraps (#1548); the heading wraps instead. */}
        <Text style={[ty.meta, { flexShrink: 0 }]} numberOfLines={1}>
          {when}
        </Text>
      </View>
      {detail ? <Text style={ty.secondary}>{detail}</Text> : null}
    </View>
  );
}
