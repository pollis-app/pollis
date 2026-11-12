// Every third-party package this build ships, with its licence (#1832); desktop's OpenSourcePage twin.

import { useEffect, useMemo, useState } from "react";
import { ActivityIndicator, FlatList, InteractionManager, Text, View } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Header, Field, ListRow } from "../../../components/ui";
import { Note } from "../../../components/self/SettingsField";
import { Icon } from "../../../components/icons";
import { ossNotices, ossPackageId } from "../../../lib/ossNotices";
import type { OssNotices, OssNoticePackage } from "../../../types/ossNotices";
import { semantic, type as ty, r, space } from "../../../theme/tokens";
import { useThemeRerender } from "../../../components/theme";

export default function OpenSource() {
  useThemeRerender();
  const { t } = useTranslation("settings");
  const router = useRouter();
  const [notices, setNotices] = useState<OssNotices | null>(null);
  const [filter, setFilter] = useState("");

  // The list is ~1 MB: load it after the drill-in animation, behind a spinner.
  useEffect(() => {
    const task = InteractionManager.runAfterInteractions(() => {
      setNotices(ossNotices());
    });
    return () => task.cancel();
  }, []);

  const visible = useMemo(() => {
    const all = notices?.packages ?? [];
    const q = filter.trim().toLowerCase();
    if (!q) {
      return all;
    }
    return all.filter((p) => p.name.toLowerCase().includes(q) || p.license.toLowerCase().includes(q));
  }, [notices, filter]);

  const header = (
    <View style={{ gap: space.lg, paddingBottom: space.lg }}>
      <Text style={ty.secondary}>{t("openSource.intro")}</Text>
      {notices ? (
        <Text testID="text-open-source-count" style={[ty.secondary, { color: semantic.dim }]}>
          {t("openSource.count", { count: notices.packages.length })}
        </Text>
      ) : null}
      <Field
        testID="input-open-source-filter"
        accessibilityLabel={t("openSource.filterLabel")}
        placeholder={t("openSource.filterPlaceholder")}
        value={filter}
        onChangeText={setFilter}
        editable={notices !== null}
        icon={<Icon.search size={16} color={semantic.dim} />}
      />
    </View>
  );

  const last = visible.length - 1;
  const renderItem = ({ item, index }: { item: OssNoticePackage; index: number }) => (
    <View
      style={{
        backgroundColor: semantic.raised,
        borderTopLeftRadius: index === 0 ? r.lg : 0,
        borderTopRightRadius: index === 0 ? r.lg : 0,
        borderBottomLeftRadius: index === last ? r.lg : 0,
        borderBottomRightRadius: index === last ? r.lg : 0,
        overflow: "hidden",
      }}
    >
      {index > 0 ? <View style={{ height: 1, backgroundColor: semantic.hairSoft }} /> : null}
      <ListRow
        testID={`row-open-source-${item.name}`}
        name={item.name}
        sub={`${item.version} · ${item.license}`}
        chevron
        onPress={() => router.push({ pathname: "/self/open-source/[id]", params: { id: ossPackageId(item) } })}
      />
    </View>
  );

  return (
    <Screen testID="screen-self-open-source">
      <Header title={t("openSource.title")} backTo={t("mobile:self.title")} />
      <FlatList
        testID="list-open-source"
        data={visible}
        keyExtractor={ossPackageId}
        renderItem={renderItem}
        extraData={last}
        ListHeaderComponent={header}
        ListEmptyComponent={
          notices ? (
            <Note testID="open-source-empty">{t("openSource.empty")}</Note>
          ) : (
            <ActivityIndicator testID="open-source-loading" color={semantic.dim} />
          )
        }
        keyboardShouldPersistTaps="handled"
        initialNumToRender={20}
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
