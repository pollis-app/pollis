import { useState } from "react";
import { ScrollView, View, Text, useWindowDimensions } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Header,
  Group,
  Card,
  ListRow,
  Chip,
  Button,
  Field,
  ActionRow,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { ErrorText, Note } from "../../components/self/SettingsField";
import { ChoiceGrid } from "../../components/self/ChoiceGrid";
import { ChoiceChip } from "../../components/self/ChoiceChip";
import { confirmSignOut } from "../../components/self/confirmSignOut";
import { useSectionScroll } from "../../components/self/useSectionScroll";
import { semantic, type as ty, fonts, space } from "../../theme/tokens";
import i18n, { activeLocale } from "../../i18n";
import {
  useUserDevices,
  useRevokeDevice,
  useLogout,
  usePendingEnrollmentRequests,
  useApproveEnrollment,
  useRejectEnrollment,
  useIdentity,
  useSecurityEvents,
  type SecurityEvent,
} from "../../hooks/queries";
import {
  AUTO_LOCK_OPTIONS_MINUTES,
  autoLockLabel,
  useAutoLockMinutes,
  useLockNow,
} from "../../lib/autolock";
import { ExportArchive } from "../../components/ExportArchive";
import { SAS_LENGTH, normalizeSasInput } from "../../lib/enrollmentSas";

function formatRelative(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) {
    return iso;
  }
  const diffMs = Date.now() - d.getTime();
  const sec = Math.floor(diffMs / 1000);
  if (sec < 60) {
    return i18n.t("mobile:self.security.justNow");
  }
  const min = Math.floor(sec / 60);
  if (min < 60) {
    return i18n.t("mobile:self.security.minutesAgo", { count: min });
  }
  const hr = Math.floor(min / 60);
  if (hr < 48) {
    return i18n.t("mobile:self.security.hoursAgo", { count: hr });
  }
  const day = Math.floor(hr / 24);
  if (day < 30) {
    return i18n.t("mobile:self.security.daysAgo", { count: day });
  }
  return d.toLocaleDateString(activeLocale(), {
    month: "short",
    day: "numeric",
  });
}

function shortId(id: string): string {
  if (id.length <= 10) {
    return id;
  }
  return `${id.slice(0, 6)}…${id.slice(-4)}`;
}

// The two addresses in an `email_changed` row's metadata, or null when the row
// does not carry the expected JSON object. Never throws — the metadata column
// is free-form, and a row from another build must render with vaguer copy
// rather than crash the screen.
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

// Human-readable summary per `security_event.kind` — mirrors desktop's
// SecurityPage `describe()`. Unknown kinds fall through to the raw string so
// new event types are never silently dropped.
function describeEvent(event: SecurityEvent): {
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
      // `name=<device name>` when the revoked row carried one (#947). This
      // event is the last place the revoked device's name is readable — the
      // device row itself is deleted on revoke.
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
      // DS-authored, inside the rotation transaction. `credential=session` is
      // the pre-enrollment soft reset (email code only — the DS also wiped
      // memberships and other devices); `credential=signature` is a rotation
      // from an enrolled device.
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
    default:
      return {
        heading: event.kind,
        detail: event.metadata ?? "",
      };
  }
}

// How many events to render before "Show older events" — the fetch is capped
// at 100 newest-first in the hook, so this is a display slice, not a query.
const SECURITY_EVENTS_PAGE_SIZE = 20;

// Group a key string into 4-char blocks so the mono line wraps at readable
// boundaries instead of mid-token.
function groupKey(key: string): string {
  return key.replace(/(.{4})/g, "$1 ").trim();
}

export default function Security() {
  const { t } = useTranslation("settings");
  const router = useRouter();
  const { section } = useLocalSearchParams<{ section?: string }>();
  const { scrollRef, sectionLayout } = useSectionScroll(section);
  const { data: devices = [], isLoading, isError } = useUserDevices();
  const revoke = useRevokeDevice();
  const logout = useLogout();
  const { data: pendingEnrollments = [] } = usePendingEnrollmentRequests();
  const approveEnrollment = useApproveEnrollment();
  const rejectEnrollment = useRejectEnrollment();
  const [confirmRevoke, setConfirmRevoke] = useState<string | null>(null);
  // The code the approver TYPES off the new device's screen, per request
  // (#1096). Never pre-filled — the server's copy is no longer even sent to the
  // client, because submitting it back would put the server on both sides of the
  // comparison the SAS exists to make.
  const [typedCodes, setTypedCodes] = useState<Record<string, string>>({});
  const { minutes: autoLockMinutes, setMinutes: setAutoLockMinutes } =
    useAutoLockMinutes();
  // Auto-lock options sit in an even grid: three across at default text
  // sizes, fewer as Dynamic Type grows, so labels never squeeze or strand.
  const { fontScale } = useWindowDimensions();
  const autoLockColumns = fontScale >= 1.6 ? 1 : fontScale >= 1.25 ? 2 : 3;
  const lockNow = useLockNow();
  const { data: identity } = useIdentity();
  const { data: events = [], isError: eventsError } = useSecurityEvents();
  const [visibleEvents, setVisibleEvents] = useState(SECURITY_EVENTS_PAGE_SIZE);

  const onRevoke = (deviceId: string) => {
    if (confirmRevoke !== deviceId) {
      setConfirmRevoke(deviceId);
      return;
    }
    revoke.mutate(deviceId, {
      onSuccess: () => setConfirmRevoke(null),
      onError: () => setConfirmRevoke(null),
    });
  };

  const onSignOut = () => {
    if (logout.isPending) {
      return;
    }
    confirmSignOut(() => {
      logout.mutate(undefined, {
        onSuccess: () => router.replace("/(auth)/email"),
        onError: () => router.replace("/(auth)/email"),
      });
    });
  };

  const sectionGap = { gap: space.md };

  return (
    <Screen testID="screen-self-security" centered>
      <Header title={t("security.title")} backTo={t("mobile:self.title")} />
      <ScrollView
        ref={scrollRef}
        style={{ flex: 1 }}
        keyboardShouldPersistTaps="handled"
        contentContainerStyle={{
          paddingHorizontal: space.xxl,
          paddingTop: space.xxl,
          paddingBottom: space.xxxl,
          gap: space.xxxl,
        }}
      >
        {/* ── Part 1: this account's devices and keys ── */}
        {pendingEnrollments.length > 0 ? (
          <View style={sectionGap}>
            <Group title={t("mobile:self.security.pairHeading")}>
              {pendingEnrollments.map((req) => {
                const typed = typedCodes[req.request_id] ?? "";
                return (
                  <View key={req.request_id} style={{ padding: space.xxl, gap: space.md }}>
                    <Text style={ty.body}>{t("mobile:self.security.pairIntro")}</Text>
                    <Field
                      testID={`input-approval-code-${req.request_id}`}
                      accessibilityLabel={t("auth:approval.codeLabel")}
                      value={typed}
                      onChangeText={(next) =>
                        setTypedCodes((prev) => ({
                          ...prev,
                          [req.request_id]: normalizeSasInput(next),
                        }))
                      }
                      placeholder={"·".repeat(SAS_LENGTH)}
                      editable={!approveEnrollment.isPending}
                      style={{ fontFamily: fonts.mono400, letterSpacing: 2 }}
                    />
                    <Text style={ty.meta}>{t("mobile:self.security.pairHint")}</Text>
                    <View style={{ flexDirection: "row", flexWrap: "wrap", gap: space.sm }}>
                      <Button
                        testID={`btn-reject-${req.request_id}`}
                        accessibilityLabel={t("mobile:self.security.rejectA11y")}
                        onPress={() => rejectEnrollment.mutate(req.request_id)}
                      >
                        {t("mobile:self.security.reject")}
                      </Button>
                      <Button
                        variant="primary"
                        testID={`btn-approve-${req.request_id}`}
                        accessibilityLabel={t("mobile:self.security.approveA11y")}
                        disabled={typed.length !== SAS_LENGTH}
                        onPress={() =>
                          approveEnrollment.mutate({
                            requestId: req.request_id,
                            verificationCode: typed,
                          })
                        }
                      >
                        {approveEnrollment.isPending
                          ? t("auth:approval.approving")
                          : t("mobile:self.security.approve")}
                      </Button>
                    </View>
                  </View>
                );
              })}
            </Group>
            {approveEnrollment.isError || rejectEnrollment.isError ? (
              <ErrorText>
                {((approveEnrollment.error ?? rejectEnrollment.error) as Error).message ||
                  t("mobile:self.security.enrollmentFailed")}
              </ErrorText>
            ) : null}
          </View>
        ) : null}

        <View style={sectionGap} onLayout={sectionLayout("devices")}>
          <Group title={t("security.devicesHeading")}>
            {/* Link a new device by QR (#1207): its own screen, one step at a time. */}
            <ListRow
              testID="row-link-device"
              glyph={<Icon.plus size={22} color={semantic.text} />}
              name={t("linkDevice.heading")}
              sub={t("linkDevice.rowSub")}
              chevron
              onPress={() => router.push("/self/link-device")}
            />
            {devices.map((d) => {
              const name =
                (d.device_name && d.device_name.trim()) ||
                d.device_id.slice(0, 8);
              const sub = t("mobile:self.security.deviceSub", {
                paired: formatRelative(d.created_at),
                lastSeen: formatRelative(d.last_seen),
              });
              const armed = confirmRevoke === d.device_id;
              const shownName = d.is_current
                ? t("mobile:self.security.thisDevice", { name })
                : name;
              return (
                <ActionRow
                  key={d.device_id}
                  testID={`row-device-${d.device_id}`}
                  glyph={<Icon.device size={22} color={semantic.dim} />}
                  name={shownName}
                  sub={sub}
                  action={
                    d.is_current ? (
                      <Chip selected>{t("mobile:self.security.current")}</Chip>
                    ) : (
                      <Chip
                        variant={armed ? "solid" : "outline"}
                        testID={`btn-revoke-device-${d.device_id}`}
                        accessibilityLabel={
                          armed
                            ? `${t("security.revokeNowConfirm")}: ${t("security.revokeConfirmSubmit")} ${name}`
                            : `${t("security.revokeConfirmSubmit")} ${name}`
                        }
                        onPress={() => onRevoke(d.device_id)}
                      >
                        {revoke.isPending && armed
                          ? t("security.revoking")
                          : armed
                            ? t("security.revokeNowConfirm")
                            : t("security.revokeButton")}
                      </Chip>
                    )
                  }
                />
              );
            })}
          </Group>
          {isLoading ? <Note>{t("security.devicesLoading")}</Note> : null}
          {isError ? <ErrorText>{t("security.devicesLoadFailed")}</ErrorText> : null}
          {revoke.isError ? (
            <ErrorText>{(revoke.error as Error).message || t("security.revokeFailed")}</ErrorText>
          ) : null}
        </View>

        <View onLayout={sectionLayout("identity")}>
          <Group title={t("mobile:self.identityHeading")}>
            <View style={{ padding: space.xxl, gap: space.md }}>
              <Text style={ty.secondary}>{t("mobile:self.security.identityDescription")}</Text>
              {identity && identity.public_key ? (
                <Text
                  testID="text-identity-key"
                  selectable
                  style={{
                    fontFamily: fonts.mono400,
                    fontSize: 13,
                    lineHeight: 20,
                    color: semantic.text,
                  }}
                >
                  {groupKey(identity.public_key)}
                </Text>
              ) : (
                <Text style={[ty.secondary, { fontFamily: fonts.mono400, color: semantic.muted }]}>
                  {t("mobile:self.security.identityMissing")}
                </Text>
              )}
            </View>
          </Group>
        </View>

        {/* ── Part 2: what has happened to the account ── */}
        <View style={sectionGap} onLayout={sectionLayout("events")}>
          <Group title={t("security.eventsHeading")}>
            {events.slice(0, visibleEvents).map((ev) => {
              const { heading, detail } = describeEvent(ev);
              const when = formatRelative(ev.created_at);
              return (
                <View
                  key={ev.id}
                  testID={`row-security-event-${ev.id}`}
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
                    <Text style={{ flex: 1, fontFamily: fonts.medium, fontSize: 16, color: semantic.text }}>
                      {heading}
                    </Text>
                    <Text style={ty.meta}>{when}</Text>
                  </View>
                  {detail ? <Text style={ty.secondary}>{detail}</Text> : null}
                </View>
              );
            })}
          </Group>
          {eventsError ? <ErrorText>{t("security.eventsLoadFailed")}</ErrorText> : null}
          {!eventsError && events.length === 0 ? (
            <Note>{t("mobile:self.security.eventsEmpty")}</Note>
          ) : null}
          {events.length > visibleEvents ? (
            <View style={{ alignItems: "flex-start" }}>
              <Button
                testID="btn-show-older-events"
                accessibilityLabel={t("mobile:self.security.showOlderA11y")}
                onPress={() => setVisibleEvents((n) => n + SECURITY_EVENTS_PAGE_SIZE)}
              >
                {t("security.eventsShowOlder", {
                  count: events.length - visibleEvents,
                })}
              </Button>
            </View>
          ) : null}
        </View>

        {/* ── Part 3: protecting and leaving the account ── */}
        <View onLayout={sectionLayout("safety")}>
          <Group title={t("mobile:self.security.safetyHeading")}>
            <ListRow
              testID="row-blocked-users"
              glyph={<Icon.ban size={22} color={semantic.text} />}
              name={t("mobile:self.security.blockedUsers")}
              chevron
              onPress={() => router.push("/self/blocked")}
            />
          </Group>
        </View>

        <View style={sectionGap} onLayout={sectionLayout("autolock")}>
          <Group title={t("security.autoLockHeading")}>
            <View style={{ padding: space.xxl, gap: space.md }}>
              <Text style={ty.secondary}>{t("mobile:self.security.autoLockDescription")}</Text>
              <ChoiceGrid
                columns={autoLockColumns}
                accessibilityRole="radiogroup"
                accessibilityLabel={t("security.autoLockAriaLabel")}
              >
                {AUTO_LOCK_OPTIONS_MINUTES.map((opt) => (
                  <ChoiceChip
                    key={opt === null ? "off" : String(opt)}
                    testID={`chip-autolock-${opt === null ? "off" : opt}`}
                    label={autoLockLabel(opt)}
                    accessibilityLabel={t("mobile:self.security.autoLockA11y", {
                      label: autoLockLabel(opt),
                    })}
                    selected={autoLockMinutes === opt}
                    onPress={() => setAutoLockMinutes(opt)}
                  />
                ))}
              </ChoiceGrid>
            </View>
            <ListRow
              testID="row-lock-now"
              glyph={<Icon.lockKeyhole size={22} color={semantic.text} />}
              name={t("mobile:self.security.lockNow")}
              sub={t("mobile:self.security.lockNowSub")}
              onPress={() => void lockNow()}
            />
          </Group>
        </View>

        <View style={sectionGap} onLayout={sectionLayout("data")}>
          <Group title={t("mobile:self.security.recoveryHeading")}>
            <View style={{ padding: space.xxl }}>
              <Text style={ty.secondary}>{t("mobile:self.security.recoveryDescription")}</Text>
            </View>
          </Group>
        </View>

        <ExportArchive padded={false} />

        <View onLayout={sectionLayout("account")}>
          <Group title={t("mobile:self.hub.accountSection")}>
            <ListRow
              testID="btn-sign-out"
              glyph={<Icon.logOut size={22} color={semantic.text} />}
              name={
                logout.isPending
                  ? t("mobile:self.hub.signingOut")
                  : t("mobile:self.hub.signOut")
              }
              nameStyle={{ fontFamily: fonts.semibold }}
              disabled={logout.isPending}
              onPress={onSignOut}
            />
            <ListRow
              testID="row-delete-account"
              glyph={<Icon.trash size={22} color={semantic.text} />}
              name={t("mobile:self.deleteAccount.title")}
              nameStyle={{ fontFamily: fonts.semibold }}
              sub={t("mobile:self.security.deleteAccountSub")}
              chevron
              onPress={() => router.push("/self/delete-account")}
            />
          </Group>
        </View>
      </ScrollView>
    </Screen>
  );
}
