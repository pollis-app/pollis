import { useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
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

function formatRelative(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) {
    return iso;
  }
  const diffMs = Date.now() - d.getTime();
  const sec = Math.floor(diffMs / 1000);
  if (sec < 60) {
    return "just now";
  }
  const min = Math.floor(sec / 60);
  if (min < 60) {
    return `${min}m ago`;
  }
  const hr = Math.floor(min / 60);
  if (hr < 48) {
    return `${hr}h ago`;
  }
  const day = Math.floor(hr / 24);
  if (day < 30) {
    return `${day}d ago`;
  }
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
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
        heading: "Device enrolled",
        detail: event.device_id
          ? `A new device (${shortId(event.device_id)}) was approved for your account.`
          : "A new device was approved for your account.",
      };
    case "device_rejected":
      return {
        heading: "Enrollment rejected",
        detail: event.device_id
          ? `A pairing request from device ${shortId(event.device_id)} was rejected.`
          : "A device pairing request was rejected.",
      };
    case "device_revoked": {
      if (!event.device_id) {
        return {
          heading: "Device revoked",
          detail: "A device was removed from your account.",
        };
      }
      // `name=<device name>` when the revoked row carried one (#947). This
      // event is the last place the revoked device's name is readable — the
      // device row itself is deleted on revoke.
      const name = event.metadata?.startsWith("name=")
        ? event.metadata.slice("name=".length)
        : null;
      return {
        heading: "Device revoked",
        detail: name
          ? `"${name}" (${shortId(event.device_id)}) was removed from your account.`
          : `Device ${shortId(event.device_id)} was removed from your account.`,
      };
    }
    case "identity_reset":
      return {
        heading: "Identity reset",
        detail:
          "Your account identity was reset with the recovery key. Prior devices were signed out.",
      };
    case "secret_key_rotated":
      return {
        heading: "Recovery key rotated",
        detail: "A new recovery key was generated. Older keys no longer work.",
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
      <Crumb segs={[{ label: "SELF" }, { label: "Security", leaf: true }]} />
      <Body>
        {pendingEnrollments.length > 0 ? (
          <View>
            <SectionTitle>PAIR NEW DEVICE</SectionTitle>
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
                  A new device wants to pair with your account.
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
                  Confirm this code matches what's shown on the other device,
                  then approve.
                </Text>
                <View style={{ flexDirection: "row", gap: 8, paddingTop: 6 }}>
                  <Chip
                    testID={`btn-reject-${req.request_id}`}
                    accessibilityLabel="Reject enrollment"
                    onPress={() => rejectEnrollment.mutate(req.request_id)}
                  >
                    Reject
                  </Chip>
                  <Chip
                    variant="on"
                    testID={`btn-approve-${req.request_id}`}
                    accessibilityLabel="Approve enrollment"
                    onPress={() =>
                      approveEnrollment.mutate({
                        requestId: req.request_id,
                        verificationCode: req.verification_code,
                      })
                    }
                  >
                    {approveEnrollment.isPending ? "Approving…" : "Approve"}
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
                  .message || "Couldn't process the enrollment request."}
              </Text>
            ) : null}
          </View>
        ) : null}

        <SectionTitle>IDENTITY</SectionTitle>
        <View style={{ paddingHorizontal: 18, paddingTop: 6, gap: 8 }}>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.mute,
              lineHeight: 17,
            }}
          >
            Your public identity key. Peers you talk to can compare this
            against what their device sees to verify it's really you.
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
              No identity key published from this device yet.
            </Text>
          )}
        </View>

        <SectionTitle>DEVICES</SectionTitle>
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
            Loading devices…
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
            Couldn't load devices.
          </Text>
        ) : null}
        {devices.map((d) => {
          const name =
            (d.device_name && d.device_name.trim()) ||
            d.device_id.slice(0, 8);
          const sub = `paired ${formatRelative(d.created_at)} · last seen ${formatRelative(d.last_seen)}`;
          const armed = confirmRevoke === d.device_id;
          return (
            <ListRow
              key={d.device_id}
              testID={`row-device-${d.device_id}`}
              minHeight={54}
              glyph={<Icon.device color={semantic.mute} />}
              name={`${name}${d.is_current ? " · this device" : ""}`}
              nameStyle={{ fontSize: 14 }}
              sub={sub}
              end={
                d.is_current ? (
                  <Chip variant="on">CURRENT</Chip>
                ) : (
                  <Chip
                    variant={armed ? "on" : "default"}
                    testID={`btn-revoke-device-${d.device_id}`}
                    accessibilityLabel="Revoke device"
                    onPress={() => onRevoke(d.device_id)}
                  >
                    {revoke.isPending && armed
                      ? "Revoking…"
                      : armed
                        ? "Confirm"
                        : "Revoke"}
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
            {(revoke.error as Error).message || "Couldn't revoke device."}
          </Text>
        ) : null}

        <SectionTitle>SECURITY EVENTS</SectionTitle>
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
            Couldn't load security events.
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
            No security events yet. Device pairings, revocations, and identity
            resets will show up here.
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
              accessibilityLabel="Show older events"
              onPress={() =>
                setVisibleEvents((n) => n + SECURITY_EVENTS_PAGE_SIZE)
              }
            >
              Show older events
            </Chip>
          </View>
        ) : null}

        <SectionTitle>SAFETY</SectionTitle>
        <ListRow
          testID="row-blocked-users"
          minHeight={48}
          glyph={<Icon.exit color={semantic.mute} />}
          name="Blocked users"
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          onPress={() => router.push("/self/blocked")}
          end={<Icon.fwd color={semantic.mute} />}
        />

        <SectionTitle>AUTO-LOCK</SectionTitle>
        <View style={{ paddingHorizontal: 18, paddingTop: 6, gap: 10 }}>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.mute,
              lineHeight: 17,
            }}
          >
            Lock Pollis behind your device PIN after a period of inactivity.
            This setting stays on this phone.
          </Text>
          <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 8 }}>
            {AUTO_LOCK_OPTIONS_MINUTES.map((opt) => (
              <Chip
                key={opt === null ? "off" : String(opt)}
                testID={`chip-autolock-${opt === null ? "off" : opt}`}
                accessibilityLabel={`Auto-lock ${autoLockLabel(opt)}`}
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
          name="Lock now"
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          sub="Require your PIN to reopen Pollis"
          onPress={() => void lockNow()}
          end={<Icon.fwd color={semantic.mute} />}
        />

        <SectionTitle>RECOVERY</SectionTitle>
        <View style={{ paddingHorizontal: 18, paddingTop: 6 }}>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.mute,
              lineHeight: 17,
            }}
          >
            Recovery key and device PIN management aren't wired on mobile
            yet. To set up a new device, sign in with your email — Pollis
            walks you through enrollment.
          </Text>
        </View>

        <SectionTitle>ACCOUNT</SectionTitle>
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
              Delete account
            </Text>
          }
          sub="Permanently delete your account and wipe this device"
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
            {logout.isPending ? "SIGNING OUT…" : "SIGN OUT"}
          </Button>
        </View>
      </Body>
      <Ctx cr="SELF" name="Security" />
    </Screen>
  );
}
