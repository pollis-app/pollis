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

// Null when the metadata is not the expected JSON. Never throws: rows from other builds must render.
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
  }
  return null;
}

// Mirrors desktop's `describe()`; unknown kinds fall through to the raw string.
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
      // The last place a revoked device's name is readable (#947).
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
      // DS-authored. `credential=session` is the pre-enrollment soft reset.
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
      const addresses = parseEmailChangeMetadata(event.metadata);
      return {
        heading: i18n.t("settings:security.eventEmailChangedHeading"),
        detail: addresses
          ? i18n.t("settings:security.eventEmailChangedDetail", addresses)
          : i18n.t("settings:security.eventEmailChangedDetailUnknown"),
      };
    }
    case "recovery_blob_fetched":
      // The first step of Secret-Key recovery.
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
