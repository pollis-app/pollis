import { useEffect, useRef, useState } from "react";
import { ActivityIndicator, Text, View } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Body, Button, BottomAction, Group } from "../components/ui";
import { Heading } from "../components/auth/Heading";
import { AuthError } from "../components/auth/AuthError";
import { RadioRow } from "../components/RadioRow";
import { HistoryTransferStatus } from "../components/self/HistoryTransferStatus";
import { semantic, type as ty, space } from "../theme/tokens";
import { formatRelative } from "../lib/relativeTime";
import {
  currentTransfer,
  isSupported,
  isTerminal,
  preferredSource,
  sortSources,
} from "../lib/historyTransfer";
import {
  takeLinkOffer,
  useHistoryTransfers,
  useRequestHistoryTransfer,
  useTransferSources,
} from "../hooks/queries/useHistoryTransfer";
import { useThemeRerender } from "../components/theme";

/**
 * New device, right after it was linked (#1735): "Copy your message history from <device>?". Copy asks that
 * device (it approves there), then this page follows the copy; the user may continue into the app at any time.
 */
export default function HistoryOffer() {
  useThemeRerender();
  const { t } = useTranslation("mobile");
  const router = useRouter();
  const [offer] = useState(() => takeLinkOffer());
  const sources = useTransferSources();
  const { data: transfers = [] } = useHistoryTransfers();
  const request = useRequestHistoryTransfer();
  const [picked, setPicked] = useState<string | null>(null);
  const [requestedId, setRequestedId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const left = useRef(false);

  const enterApp = () => {
    if (left.current) {
      return;
    }
    left.current = true;
    router.replace("/(tabs)/groups");
  };

  const usable = sortSources(sources.data ?? []).filter(isSupported);
  const preferred = preferredSource(sources.data ?? [], offer?.approverDeviceId ?? null);
  const chosen = preferred ?? usable.find((s) => s.device_id === picked) ?? null;

  // Nothing to copy from (a recovered account with no other device, an older sibling): straight in.
  useEffect(() => {
    if (sources.isSuccess && usable.length === 0 && !requestedId) {
      enterApp();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sources.isSuccess, usable.length]);

  const following = requestedId
    ? (transfers.find((tr) => tr.transfer_id === requestedId) ?? currentTransfer(transfers, "receiver"))
    : null;

  const onCopy = () => {
    if (!chosen || request.isPending) {
      return;
    }
    setError(null);
    request.mutate(
      { sourceDeviceId: chosen.device_id, via: "link" },
      {
        onSuccess: (transfer) => setRequestedId(transfer.transfer_id),
        onError: (e) => setError((e as Error).message || t("historyTransfer.picker.requestFailed")),
      },
    );
  };

  if (following) {
    return (
      <Screen testID="screen-history-offer">
        <Body contentContainerStyle={{ paddingHorizontal: 24, paddingTop: 52, gap: 24 }}>
          <HistoryTransferStatus transfer={following} />
          {isTerminal(following.state) ? null : (
            <Text style={[ty.meta, { color: semantic.dim }]}>{t("historyTransfer.offer.continueHint")}</Text>
          )}
        </Body>
        <BottomAction>
          <Button
            testID="btn-history-offer-continue"
            variant={isTerminal(following.state) ? "primary" : "subtle"}
            full
            onPress={enterApp}
          >
            {t("historyTransfer.offer.continue")}
          </Button>
        </BottomAction>
      </Screen>
    );
  }

  const name = (s: { device_name: string | null; device_id: string }) =>
    s.device_name?.trim() || s.device_id.slice(0, 8);

  return (
    <Screen testID="screen-history-offer">
      <Body contentContainerStyle={{ paddingHorizontal: 24, paddingTop: 52, gap: 24 }}>
        <Heading
          testID="history-offer-heading"
          title={t("historyTransfer.offer.title")}
          subtitle={
            preferred
              ? t("historyTransfer.offer.body", { device: name(preferred) })
              : t("historyTransfer.offer.pickBody")
          }
        />
        {sources.isPending ? (
          <View style={{ flexDirection: "row", alignItems: "center", gap: space.md }}>
            <ActivityIndicator color={semantic.accent} />
            <Text style={ty.secondary}>{t("historyTransfer.offer.loading")}</Text>
          </View>
        ) : null}
        {!preferred && usable.length > 1 ? (
          <View accessibilityRole="radiogroup" accessibilityLabel={t("historyTransfer.offer.pickHeading")}>
            <Group title={t("historyTransfer.offer.pickHeading")}>
              {usable.map((s) => (
                <RadioRow
                  key={s.device_id}
                  testID={`radio-history-source-${s.device_id}`}
                  label={name(s)}
                  sub={
                    s.online
                      ? t("historyTransfer.picker.online")
                      : s.last_active_at
                        ? t("historyTransfer.picker.lastActive", { when: formatRelative(s.last_active_at) })
                        : undefined
                  }
                  selected={picked === s.device_id}
                  onPress={() => setPicked(s.device_id)}
                />
              ))}
            </Group>
          </View>
        ) : null}
        {error ? <AuthError message={error} /> : null}
      </Body>
      <BottomAction>
        <Button
          testID="btn-history-offer-copy"
          variant="primary"
          full
          disabled={!chosen}
          loading={request.isPending}
          onPress={onCopy}
        >
          {t("historyTransfer.offer.copy")}
        </Button>
        <Button testID="btn-history-offer-skip" variant="subtle" full disabled={request.isPending} onPress={enterApp}>
          {t("historyTransfer.offer.notNow")}
        </Button>
      </BottomAction>
    </Screen>
  );
}
