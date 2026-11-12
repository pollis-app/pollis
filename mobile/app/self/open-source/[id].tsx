// One package's licence notice (#1832); desktop's OpenSourceLicensePage twin.

import { useEffect, useState } from "react";
import { ActivityIndicator, InteractionManager, Text, View } from "react-native";
import { useLocalSearchParams } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Header, Body } from "../../../components/ui";
import { Note } from "../../../components/self/SettingsField";
import { ossNotices, ossPackageId } from "../../../lib/ossNotices";
import type { OssNotices } from "../../../types/ossNotices";
import { semantic, type as ty, r, space } from "../../../theme/tokens";
import { useThemeRerender } from "../../../components/theme";

export default function OpenSourceLicense() {
  useThemeRerender();
  const { t } = useTranslation("settings");
  const { id } = useLocalSearchParams<{ id: string }>();
  const [notices, setNotices] = useState<OssNotices | null>(null);

  useEffect(() => {
    const task = InteractionManager.runAfterInteractions(() => {
      setNotices(ossNotices());
    });
    return () => task.cancel();
  }, []);

  const pkg = notices?.packages.find((p) => ossPackageId(p) === id);

  return (
    <Screen testID="screen-self-open-source-license">
      <Header title={pkg?.name ?? t("openSource.title")} backTo={t("openSource.title")} />
      <Body contentContainerStyle={{ paddingHorizontal: space.xxl, paddingTop: space.xxl, gap: space.lg }}>
        {!notices ? <ActivityIndicator color={semantic.dim} /> : null}
        {notices && !pkg ? <Note testID="open-source-not-found">{t("openSource.notFound")}</Note> : null}
        {notices && pkg ? (
          <>
            <View style={{ gap: space.xs }}>
              <Text style={ty.secondary}>
                <Text style={{ color: semantic.dim }}>{t("openSource.versionLabel")}: </Text>
                <Text testID="text-open-source-version">{pkg.version}</Text>
              </Text>
              <Text style={ty.secondary}>
                <Text style={{ color: semantic.dim }}>{t("openSource.licenseLabel")}: </Text>
                <Text testID="text-open-source-license">{pkg.license}</Text>
              </Text>
              <Text style={ty.secondary}>
                <Text style={{ color: semantic.dim }}>{t("openSource.sourceLabel")}: </Text>
                {pkg.ecosystem === "cargo" ? t("openSource.cargo") : t("openSource.npm")}
              </Text>
            </View>
            {pkg.texts.map((i) => (
              <View key={i} style={{ backgroundColor: semantic.raised, borderRadius: r.md, padding: space.lg }}>
                <Text testID="text-open-source-text" selectable style={[ty.mono, { color: semantic.dim, fontSize: 12 }]}>
                  {notices.texts[i]}
                </Text>
              </View>
            ))}
          </>
        ) : null}
      </Body>
    </Screen>
  );
}
