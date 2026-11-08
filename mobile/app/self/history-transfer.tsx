import { useRef, useState } from "react";
import { ActivityIndicator, ScrollView, Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Screen, Header, Group, ListRow, Dot } from "../../components/ui";
import { Icon } from "../../components/icons";
import { ErrorText, Note } from "../../components/self/SettingsField";
import { HistoryTransferStatus } from "../../components/self/HistoryTransferStatus";
import { semantic, type as ty, space } from "../../theme/tokens";
import { formatRelative } from "../../lib/relativeTime";
import {
  currentTransfer,
  isSupported,
  isTerminal,
  sortSources,
  type TransferSource,
} from "../../lib/historyTransfer";
import {
  useHistoryTransfers,
  useRequestHistoryTransfer,
  useTransferSources,
} from "../../hooks/queries";
import { useThemeRerender } from "../../components/theme";

export function sourceGlyph(source: TransferSource, color: string) {
  const desktop = source.platform && !/ios|android|iphone|ipad/i.test(source.platform);
  return desktop ? <Icon.laptop size={22} color={color} /> : <Icon.device size={22} color={color} />;
}

/** Settings → Security → Copy message history (#1735): pick one of your devices, then follow the copy. */
export default function CopyMessageHistory() {
  useThemeRerender();
  const { t } = useTranslation("mobile");
  const { data: transfers = [] } = useHistoryTransfers();
  const sources = useTransferSources();
  const request = useRequestHistoryTransfer();
  const [asking, setAsking] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The receiver transfer this visit follows; a finished one stays until "Done".
  const [dismissed, setDismissed] = useState<string | null>(null);
  // Transfers this visit started or watched running finish on screen instead of vanishing.
  const seen = useRef(new Set<string>()).current;

  const receiving = currentTransfer(transfers, "receiver");
  if (receiving && !isTerminal(receiving.state)) {
    seen.add(receiving.transfer_id);
  }
  const followed =
    receiving && receiving.transfer_id !== dismissed && seen.has(receiving.transfer_id) ? receiving : null;
  const sending = currentTransfer(transfers, "source");
  const sendingActive = sending && !isTerminal(sending.state) ? sending : null;

  const onPick = (source: TransferSource) => {
    if (asking || request.isPending || !isSupported(source)) {
      return;
    }
    setError(null);
    setAsking(source.device_id);
    request.mutate(
      { sourceDeviceId: source.device_id, via: "sibling" },
      {
        onSuccess: (transfer) => {
          seen.add(transfer.transfer_id);
          setDismissed(null);
          setAsking(null);
        },
        onError: (e) => {
          setAsking(null);
          setError((e as Error).message || t("historyTransfer.picker.requestFailed"));
        },
      },
    );
  };

  const list = sortSources(sources.data ?? []);

  return (
    <Screen testID="screen-history-transfer">
      <Header title={t("historyTransfer.picker.title")} backTo={t("settings:security.title")} />
      <ScrollView
        style={{ flex: 1 }}
        contentContainerStyle={{
          paddingHorizontal: space.xxl,
          paddingTop: space.xxl,
          paddingBottom: space.xxxl,
          gap: space.xxxl,
        }}
      >
        {sendingActive ? <HistoryTransferStatus transfer={sendingActive} /> : null}
        {followed ? (
          <HistoryTransferStatus
            transfer={followed}
            onDone={() => setDismissed(followed.transfer_id)}
          />
        ) : (
          <View style={{ gap: space.xl }}>
            <Text style={[ty.body, { color: semantic.dim }]}>{t("historyTransfer.picker.intro")}</Text>
            <Group title={t("historyTransfer.picker.heading")} testID="history-transfer-sources">
              {list.map((source) => {
                const supported = isSupported(source);
                const name = source.device_name?.trim() || source.device_id.slice(0, 8);
                const sub = !supported
                  ? t("historyTransfer.picker.unsupported")
                  : source.online
                    ? t("historyTransfer.picker.online")
                    : source.last_active_at
                      ? t("historyTransfer.picker.lastActive", { when: formatRelative(source.last_active_at) })
                      : t("historyTransfer.picker.neverActive");
                const busy = asking === source.device_id;
                return (
                  <ListRow
                    key={source.device_id}
                    testID={`row-history-source-${source.device_id}`}
                    glyph={sourceGlyph(source, supported ? semantic.text : semantic.dim)}
                    name={name}
                    sub={sub}
                    disabled={!supported || (asking !== null && !busy)}
                    chevron={!busy}
                    end={
                      busy ? (
                        <ActivityIndicator color={semantic.accent} />
                      ) : source.online && supported ? (
                        <Dot color={semantic.accent} />
                      ) : undefined
                    }
                    accessibilityLabel={busy ? t("historyTransfer.picker.requesting", { device: name }) : undefined}
                    onPress={() => onPick(source)}
                  />
                );
              })}
            </Group>
            {sources.isPending ? <Note>{t("historyTransfer.offer.loading")}</Note> : null}
            {sources.isError ? <ErrorText>{t("historyTransfer.picker.loadFailed")}</ErrorText> : null}
            {!sources.isPending && !sources.isError && list.length === 0 ? (
              <Note testID="history-transfer-empty">{t("historyTransfer.picker.empty")}</Note>
            ) : null}
            {error ? <ErrorText testID="history-transfer-request-error">{error}</ErrorText> : null}
          </View>
        )}
      </ScrollView>
    </Screen>
  );
}
