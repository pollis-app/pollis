// The whole security-event log (#1548), keyset-paged and filterable; desktop's SecurityEventsPage twin.

import { useMemo, useState } from "react";
import { ActivityIndicator, FlatList, Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Screen, Header, Chip } from "../../components/ui";
import { ErrorText, Note } from "../../components/self/SettingsField";
import { SecurityEventRow } from "../../components/self/SecurityEventRow";
import { useSecurityEventPages } from "../../hooks/queries";
import {
  SECURITY_EVENT_FILTER_ORDER,
  type SecurityEvent,
  type SecurityEventFilter,
} from "../../lib/securityEvents";
import { semantic, type as ty, r, space } from "../../theme/tokens";
import { useThemeRerender } from "../../components/theme";

const FILTER_LABEL_KEYS: Record<SecurityEventFilter, string> = {
  all: "security.eventsFilterAll",
  devices: "security.eventsFilterDevices",
  identity: "security.eventsFilterIdentity",
  account: "security.eventsFilterAccount",
};

export default function SecurityEvents() {
  useThemeRerender();
  const { t } = useTranslation("settings");
  const [filter, setFilter] = useState<SecurityEventFilter>("all");
  const {
    data,
    isPending,
    isError,
    isPlaceholderData,
    hasNextPage,
    fetchNextPage,
    isFetchingNextPage,
  } = useSecurityEventPages(filter);

  const events = useMemo(() => data?.pages.flatMap((p) => p.events) ?? [], [data]);

  const loadMore = () => {
    if (hasNextPage && !isFetchingNextPage && !isPlaceholderData) {
      void fetchNextPage();
    }
  };

  const header = (
    <View style={{ gap: space.lg, paddingBottom: space.lg }}>
      <Text style={ty.secondary}>{t("security.eventsDescription")}</Text>
      {/* Wraps, so every filter stays in view at large text sizes. */}
      <View style={{ flexDirection: "row", flexWrap: "wrap", gap: space.sm }}>
        {SECURITY_EVENT_FILTER_ORDER.map((f) => (
          <Chip
            key={f}
            testID={`chip-security-events-${f}`}
            selected={filter === f}
            onPress={() => setFilter(f)}
          >
            {t(FILTER_LABEL_KEYS[f])}
          </Chip>
        ))}
      </View>
      {isError ? <ErrorText>{t("security.eventsLoadFailed")}</ErrorText> : null}
    </View>
  );

  const empty = isPending ? (
    <ActivityIndicator color={semantic.dim} />
  ) : isError || isPlaceholderData ? null : (
    <View testID="security-events-empty">
      <Note>
        {filter === "all"
          ? t("mobile:self.security.eventsEmpty")
          : t("security.eventsEmptyFiltered")}
      </Note>
    </View>
  );

  const last = events.length - 1;
  const renderItem = ({ item, index }: { item: SecurityEvent; index: number }) => (
    <View
      style={{
        backgroundColor: semantic.raised,
        borderTopLeftRadius: index === 0 ? r.lg : 0,
        borderTopRightRadius: index === 0 ? r.lg : 0,
        borderBottomLeftRadius: index === last ? r.lg : 0,
        borderBottomRightRadius: index === last ? r.lg : 0,
        overflow: "hidden",
        // The previous filter's rows stay up, dimmed, until the new first page lands.
        opacity: isPlaceholderData ? 0.5 : 1,
      }}
    >
      {index > 0 ? <View style={{ height: 1, backgroundColor: semantic.hairSoft }} /> : null}
      <SecurityEventRow event={item} />
    </View>
  );

  return (
    <Screen testID="screen-self-security-events">
      <Header title={t("security.eventsHeading")} backTo={t("security.title")} />
      <FlatList
        testID="list-security-events"
        data={events}
        keyExtractor={(e) => e.id}
        renderItem={renderItem}
        extraData={`${last}:${isPlaceholderData}`}
        ListHeaderComponent={header}
        ListEmptyComponent={empty}
        ListFooterComponent={
          isFetchingNextPage ? (
            <View testID="security-events-loading-more" style={{ paddingVertical: space.lg }}>
              <ActivityIndicator color={semantic.dim} />
            </View>
          ) : null
        }
        onEndReached={loadMore}
        onEndReachedThreshold={0.5}
        style={{ flex: 1 }}
        contentContainerStyle={{
          paddingHorizontal: space.xxl,
          paddingTop: space.xxl,
          paddingBottom: space.xxxl,
        }}
      />
    </Screen>
  );
}
