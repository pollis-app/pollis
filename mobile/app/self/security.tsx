import { useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Chip,
  Button,
  Ctx,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty, fonts } from "../../theme/tokens";
import i18n, { activeLocale, upper } from "../../i18n";
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
    case "secret_key_rotated":
      return {
        heading: i18n.t("settings:security.eventSecretKeyRotatedHeading"),
        detail: i18n.t("settings:security.eventSecretKeyRotatedDetail"),
      };
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
  const { data: devices = [], isLoading, isError } = useUserDevices();
  const revoke = useRevokeDevice();
  const logout = useLogout();
  const { data: pendingEnrollments = [] } = usePendingEnrollmentRequests();
  const approveEnrollment = useApproveEnrollment();
  const rejectEnrollment = useRejectEnrollment();
  const [confirmRevoke, setConfirmRevoke] = useState<string | null>(null);
  const { minutes: autoLockMinutes, setMinutes: setAutoLockMinutes } =
    useAutoLockMinutes();
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
    logout.mutate(undefined, {
      onSuccess: () => router.replace("/(auth)/email"),
      onError: () => router.replace("/(auth)/email"),
    });
  };

  return (
    <Screen testID="screen-self-security" centered>
      <Crumb
        segs={[
          { label: upper(t("mobile:self.title")) },
          { label: t("security.title"), leaf: true },
        ]}
      />
      <Body>
        {pendingEnrollments.length > 0 ? (
          <View>
            <SectionTitle>
              {upper(t("mobile:self.security.pairHeading"))}
            </SectionTitle>
            {pendingEnrollments.map((req) => (
              <View
                key={req.request_id}
                style={{
                  paddingHorizontal: 18,
                  paddingVertical: 12,
                  gap: 8,
                  borderBottomWidth: 1,
                  borderBottomColor: semantic.hairSoft,
                }}
              >
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 13,
                    color: semantic.ink,
                  }}
                >
                  {t("mobile:self.security.pairIntro")}
                </Text>
                <Text
                  style={{
                    fontFamily: fonts.mono400,
                    fontSize: 18,
                    letterSpacing: 3,
                    color: semantic.accent,
                  }}
                >
                  {req.verification_code}
                </Text>
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 11,
                    color: semantic.mute,
                  }}
                >
                  {t("mobile:self.security.pairHint")}
                </Text>
                <View style={{ flexDirection: "row", gap: 8, paddingTop: 6 }}>
                  <Chip
                    testID={`btn-reject-${req.request_id}`}
                    accessibilityLabel={t("mobile:self.security.rejectA11y")}
                    onPress={() => rejectEnrollment.mutate(req.request_id)}
                  >
                    {t("mobile:self.security.reject")}
                  </Chip>
                  <Chip
                    variant="on"
                    testID={`btn-approve-${req.request_id}`}
                    accessibilityLabel={t("mobile:self.security.approveA11y")}
                    onPress={() =>
                      approveEnrollment.mutate({
                        requestId: req.request_id,
                        verificationCode: req.verification_code,
                      })
                    }
                  >
                    {approveEnrollment.isPending
                      ? t("auth:approval.approving")
                      : t("mobile:self.security.approve")}
                  </Chip>
                </View>
              </View>
            ))}
            {(approveEnrollment.isError || rejectEnrollment.isError) ? (
              <Text
                style={{
                  fontFamily: ty.body.fontFamily,
                  fontSize: 12,
                  color: semantic.danger,
                  paddingHorizontal: 18,
                  paddingTop: 6,
                }}
              >
                {((approveEnrollment.error ?? rejectEnrollment.error) as Error)
                  .message || t("mobile:self.security.enrollmentFailed")}
              </Text>
            ) : null}
          </View>
        ) : null}

        <SectionTitle>{upper(t("mobile:self.identityHeading"))}</SectionTitle>
        <View style={{ paddingHorizontal: 18, paddingTop: 6, gap: 8 }}>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.mute,
              lineHeight: 17,
            }}
          >
            {t("mobile:self.security.identityDescription")}
          </Text>
          {identity && identity.public_key ? (
            <Text
              testID="text-identity-key"
              selectable
              style={{
                fontFamily: fonts.mono400,
                fontSize: 12,
                lineHeight: 18,
                color: semantic.ink,
              }}
            >
              {groupKey(identity.public_key)}
            </Text>
          ) : (
            <Text
              style={{
                fontFamily: fonts.mono400,
                fontSize: 12,
                color: semantic.mute2,
              }}
            >
              {t("mobile:self.security.identityMissing")}
            </Text>
          )}
        </View>

        <SectionTitle>{upper(t("security.devicesHeading"))}</SectionTitle>
        {isLoading ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingVertical: 12,
            }}
          >
            {t("security.devicesLoading")}
          </Text>
        ) : null}
        {isError ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.danger,
              paddingHorizontal: 18,
              paddingVertical: 12,
            }}
          >
            {t("security.devicesLoadFailed")}
          </Text>
        ) : null}
        {devices.map((d) => {
          const name =
            (d.device_name && d.device_name.trim()) ||
            d.device_id.slice(0, 8);
          const sub = t("mobile:self.security.deviceSub", {
            paired: formatRelative(d.created_at),
            lastSeen: formatRelative(d.last_seen),
          });
          const armed = confirmRevoke === d.device_id;
          return (
            <ListRow
              key={d.device_id}
              testID={`row-device-${d.device_id}`}
              minHeight={54}
              glyph={<Icon.device color={semantic.mute} />}
              name={
                d.is_current
                  ? t("mobile:self.security.thisDevice", { name })
                  : name
              }
              nameStyle={{ fontSize: 14 }}
              sub={sub}
              end={
                d.is_current ? (
                  <Chip variant="on">
                    {upper(t("mobile:self.security.current"))}
                  </Chip>
                ) : (
                  <Chip
                    variant={armed ? "on" : "default"}
                    testID={`btn-revoke-device-${d.device_id}`}
                    accessibilityLabel={t("security.revokeConfirmSubmit")}
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
        {revoke.isError ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.danger,
              paddingHorizontal: 18,
              paddingTop: 6,
            }}
          >
            {(revoke.error as Error).message || t("security.revokeFailed")}
          </Text>
        ) : null}

        <SectionTitle>{upper(t("security.eventsHeading"))}</SectionTitle>
        {eventsError ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.danger,
              paddingHorizontal: 18,
              paddingVertical: 12,
            }}
          >
            {t("security.eventsLoadFailed")}
          </Text>
        ) : null}
        {!eventsError && events.length === 0 ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingVertical: 12,
            }}
          >
            {t("mobile:self.security.eventsEmpty")}
          </Text>
        ) : null}
        {events.slice(0, visibleEvents).map((ev) => {
          const { heading, detail } = describeEvent(ev);
          return (
            <View
              key={ev.id}
              testID={`row-security-event-${ev.id}`}
              style={{
                paddingHorizontal: 18,
                paddingVertical: 10,
                gap: 2,
                borderBottomWidth: 1,
                borderBottomColor: semantic.hairSoft,
              }}
            >
              <View
                style={{
                  flexDirection: "row",
                  alignItems: "center",
                  gap: 8,
                }}
              >
                <Text
                  style={{
                    fontFamily: ty.rowN.fontFamily,
                    fontSize: 13,
                    color: semantic.ink,
                    flex: 1,
                  }}
                >
                  {heading}
                </Text>
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 11,
                    color: semantic.mute2,
                  }}
                >
                  {formatRelative(ev.created_at)}
                </Text>
              </View>
              {detail ? (
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 12,
                    lineHeight: 17,
                    color: semantic.mute,
                  }}
                >
                  {detail}
                </Text>
              ) : null}
            </View>
          );
        })}
        {events.length > visibleEvents ? (
          <View
            style={{
              paddingHorizontal: 18,
              paddingTop: 10,
              flexDirection: "row",
            }}
          >
            <Chip
              testID="btn-show-older-events"
              accessibilityLabel={t("mobile:self.security.showOlderA11y")}
              onPress={() =>
                setVisibleEvents((n) => n + SECURITY_EVENTS_PAGE_SIZE)
              }
            >
              {t("security.eventsShowOlder", {
                count: events.length - visibleEvents,
              })}
            </Chip>
          </View>
        ) : null}

        <SectionTitle>{upper(t("mobile:self.security.safetyHeading"))}</SectionTitle>
        <ListRow
          testID="row-blocked-users"
          minHeight={48}
          glyph={<Icon.exit color={semantic.mute} />}
          name={t("nav:breadcrumb.blockedUsers")}
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          onPress={() => router.push("/self/blocked")}
          end={<Icon.fwd color={semantic.mute} />}
        />

        <SectionTitle>{upper(t("security.autoLockHeading"))}</SectionTitle>
        <View style={{ paddingHorizontal: 18, paddingTop: 6, gap: 10 }}>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.mute,
              lineHeight: 17,
            }}
          >
            {t("mobile:self.security.autoLockDescription")}
          </Text>
          <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
            {AUTO_LOCK_OPTIONS_MINUTES.map((opt) => (
              <Chip
                key={opt === null ? "off" : String(opt)}
                testID={`chip-autolock-${opt === null ? "off" : opt}`}
                accessibilityLabel={t("mobile:self.security.autoLockA11y", {
                  label: autoLockLabel(opt),
                })}
                variant={autoLockMinutes === opt ? "on" : "default"}
                onPress={() => setAutoLockMinutes(opt)}
              >
                {autoLockLabel(opt)}
              </Chip>
            ))}
          </View>
        </View>
        <ListRow
          testID="row-lock-now"
          minHeight={48}
          glyph={<Icon.lock color={semantic.mute} />}
          name={t("mobile:self.security.lockNow")}
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          sub={t("mobile:self.security.lockNowSub")}
          onPress={() => void lockNow()}
          end={<Icon.fwd color={semantic.mute} />}
        />

        <SectionTitle>{upper(t("mobile:self.security.recoveryHeading"))}</SectionTitle>
        <View style={{ paddingHorizontal: 18, paddingTop: 6 }}>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.mute,
              lineHeight: 17,
            }}
          >
            {t("mobile:self.security.recoveryDescription")}
          </Text>
        </View>

        <ExportArchive />

        <SectionTitle>{upper(t("user.accountHeading"))}</SectionTitle>
        <ListRow
          testID="row-delete-account"
          minHeight={48}
          glyph={<Icon.shield color={semantic.danger} />}
          name={
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 14,
                color: semantic.danger,
              }}
            >
              {t("mobile:self.deleteAccount.title")}
            </Text>
          }
          sub={t("mobile:self.security.deleteAccountSub")}
          onPress={() => router.push("/self/delete-account")}
          end={<Icon.fwd color={semantic.mute} />}
        />

        <View style={{ paddingHorizontal: 18, paddingTop: 18 }}>
          <Button
            full
            testID="btn-sign-out"
            variant="danger"
            icon={<Icon.exit color={semantic.danger} />}
            onPress={onSignOut}
            disabled={logout.isPending}
          >
            {logout.isPending
              ? upper(t("mobile:self.hub.signingOut"))
              : upper(t("auth:shell.signOutTitle"))}
          </Button>
        </View>
      </Body>
      <Ctx cr={upper(t("mobile:self.title"))} name={t("security.title")} />
    </Screen>
  );
}
