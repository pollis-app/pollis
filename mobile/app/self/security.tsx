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
} from "../../hooks/queries";

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

export default function Security() {
  const router = useRouter();
  const { data: devices = [], isLoading, isError } = useUserDevices();
  const revoke = useRevokeDevice();
  const logout = useLogout();
  const { data: pendingEnrollments = [] } = usePendingEnrollmentRequests();
  const approveEnrollment = useApproveEnrollment();
  const rejectEnrollment = useRejectEnrollment();
  const [confirmRevoke, setConfirmRevoke] = useState<string | null>(null);

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
