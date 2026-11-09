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
import {
  useUserDevices,
  useRevokeDevice,
  useLogout,
  usePendingEnrollmentRequests,
  useApproveEnrollment,
  useRejectEnrollment,
  useSecurityEventsPreview,
  useHistoryTransfers,
  usePendingTransferRequests,
  useDeviceUnlock,
} from "../../hooks/queries";
import { ToggleRow } from "../../components/self/ToggleRow";
import { biometricMethod, deviceUnlockSupported } from "../../lib/deviceUnlock";
import { showSwitch } from "../../lib/deviceUnlockRules";
import { currentTransfer, isTerminal } from "../../lib/historyTransfer";
import { SecurityEventRow } from "../../components/self/SecurityEventRow";
import { formatRelative } from "../../lib/relativeTime";
import {
  AUTO_LOCK_OPTIONS_MINUTES,
  autoLockLabel,
  useAutoLockMinutes,
  useLockNow,
} from "../../lib/autolock";
import { ExportArchive } from "../../components/ExportArchive";
import { SAS_LENGTH, normalizeSasInput } from "../../lib/enrollmentSas";
import { haptic } from "../../lib/haptics";
import { useThemeRerender } from "../../components/theme";

export default function Security() {
  useThemeRerender();
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
  // The code the approver TYPES (#1096); never pre-filled.
  const [typedCodes, setTypedCodes] = useState<Record<string, string>>({});
  const { minutes: autoLockMinutes, setMinutes: setAutoLockMinutes } =
    useAutoLockMinutes();
  // An even grid that loses columns as Dynamic Type grows.
  const { fontScale } = useWindowDimensions();
  const autoLockColumns = fontScale >= 1.6 ? 1 : fontScale >= 1.25 ? 2 : 3;
  const lockNow = useLockNow();
  const deviceUnlock = useDeviceUnlock();
  const biometricSupported = deviceUnlockSupported();
  const { data: transfers = [] } = useHistoryTransfers();
  // #1735: a request backed out of on its approval page stays answerable from here.
  const { data: pendingTransfers = [] } = usePendingTransferRequests();
  // #1735: an unfinished copy shows on the row, either direction.
  const liveTransfer = [currentTransfer(transfers, "receiver"), currentTransfer(transfers, "source")].find(
    (tr) => tr && !isTerminal(tr.state),
  );
  const transferDevice = liveTransfer?.peer_device_name?.trim() || t("mobile:historyTransfer.approve.unknownDevice");
  const transferSub = !liveTransfer
    ? t("mobile:historyTransfer.security.rowSub")
    : liveTransfer.state === "paused"
      ? t("mobile:historyTransfer.security.rowPaused", { device: transferDevice })
      : liveTransfer.role === "receiver"
        ? t("mobile:historyTransfer.security.rowActive", { device: transferDevice })
        : t("mobile:historyTransfer.security.rowSending", { device: transferDevice });
  const hasOtherDevices = devices.some((d) => !d.is_current);
  const {
    data: events = [],
    isError: eventsError,
    isPending: eventsLoading,
  } = useSecurityEventsPreview();

  const onRevoke = (deviceId: string) => {
    if (confirmRevoke !== deviceId) {
      haptic("confirmArm");
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
      logout.mutate();
    });
  };

  const sectionGap = { gap: space.md };

  return (
    <Screen testID="screen-self-security">
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
                      surface="raised"
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
                        surface="raised"
                        accessibilityLabel={t("mobile:self.security.rejectA11y")}
                        onPress={() => rejectEnrollment.mutate(req.request_id)}
                      >
                        {t("mobile:self.security.reject")}
                      </Button>
                      <Button
                        variant="primary"
                        testID={`btn-approve-${req.request_id}`}
                        surface="raised"
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
            <ListRow
              testID="row-link-device"
              glyph={<Icon.plus size={22} color={semantic.text} />}
              name={t("mobile:self.scanDevice.heading")}
              sub={t("mobile:self.scanDevice.rowSub")}
              chevron
              onPress={() => router.push("/self/link-device")}
            />
            {hasOtherDevices || liveTransfer ? (
              <ListRow
                testID="row-copy-history"
                glyph={<Icon.history size={22} color={semantic.text} />}
                name={t("mobile:historyTransfer.security.row")}
                sub={transferSub}
                chevron
                onPress={() => router.push("/self/history-transfer")}
              />
            ) : null}
            {pendingTransfers.map((req) => (
              <ListRow
                key={req.transfer_id}
                testID={`row-history-request-${req.transfer_id}`}
                glyph={<Icon.history size={22} color={semantic.accent} />}
                name={t("mobile:historyTransfer.security.pendingRow", {
                  device: req.requester_device_name?.trim() || t("mobile:historyTransfer.approve.unknownDevice"),
                })}
                sub={t("mobile:historyTransfer.security.pendingRowSub")}
                chevron
                onPress={() =>
                  router.push({ pathname: "/approve-history-transfer", params: { transferId: req.transfer_id } })
                }
              />
            ))}
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
                      // Status, not a control.
                      <Text style={[ty.section, { color: semantic.accent }]}>
                        {t("mobile:self.security.current")}
                      </Text>
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

        {/* ── Part 2: what has happened to the account ── */}
        <View style={sectionGap} onLayout={sectionLayout("events")}>
          <Group title={t("security.eventsHeading")}>
            {/* Only the newest few (#1548). */}
            {events.map((ev) => (
              <SecurityEventRow key={ev.id} event={ev} />
            ))}
            {events.length > 0 ? (
              <ListRow
                testID="row-security-events-all"
                name={t("security.eventsViewAll")}
                chevron
                onPress={() => router.push("/self/security-events")}
              />
            ) : null}
          </Group>
          {eventsError ? <ErrorText>{t("security.eventsLoadFailed")}</ErrorText> : null}
          {!eventsError && !eventsLoading && events.length === 0 ? (
            <Note>{t("mobile:self.security.eventsEmpty")}</Note>
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

        {/* The PIN, and biometrics as a stand-in for typing it at unlock (#1685). */}
        <View style={sectionGap} onLayout={sectionLayout("pin")}>
          <Group title={t("security.pinHeading")}>
            <ListRow
              testID="row-change-pin"
              glyph={<Icon.key size={22} color={semantic.text} />}
              name={t("security.changePinButton")}
              sub={t("mobile:self.security.changePinSub")}
              chevron
              onPress={() => router.push("/self/change-pin")}
            />
            {showSwitch(biometricSupported, deviceUnlock.enabled) ? (
              <ToggleRow
                testID="toggle-biometric-unlock"
                label={t("mobile:biometric.switchLabel", { method: biometricMethod() })}
                sub={
                  biometricSupported
                    ? t("mobile:biometric.switchSub")
                    : t("mobile:biometric.unavailableSub", { method: biometricMethod() })
                }
                on={deviceUnlock.enabled}
                disabled={deviceUnlock.isLoading || deviceUnlock.disable.isPending}
                onToggle={() => {
                  if (deviceUnlock.enabled) {
                    deviceUnlock.disable.mutate();
                  } else {
                    router.push("/self/biometric-unlock");
                  }
                }}
              />
            ) : null}
          </Group>
          {deviceUnlock.disable.isError ? (
            <ErrorText>
              {(deviceUnlock.disable.error as Error).message || t("mobile:biometric.failed")}
            </ErrorText>
          ) : null}
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

        <View onLayout={sectionLayout("data")}>
          <ExportArchive padded={false} />
        </View>

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
