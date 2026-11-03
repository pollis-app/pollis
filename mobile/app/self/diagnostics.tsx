import { useEffect, useState } from "react";
import { Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Screen, Header, Body } from "../../components/ui";
import { CopyDiagnosticsButton } from "../../components/self/CopyDiagnosticsButton";
import { SaveDiagnosticsButton } from "../../components/self/SaveDiagnosticsButton";
import { ErrorText } from "../../components/self/SettingsField";
import { semantic, type as ty, space, r } from "../../theme/tokens";
import { getDiagnostics } from "../../lib/diagnostics";
import { recentReport, type RecentReport } from "../../lib/diagnosticsReport";
import { useThemeRerender } from "../../components/theme";

/** The on-device diagnostics log (#1305): its newest part, exactly what Copy takes; the whole log saves as a file. Desktop's DiagnosticsPage. */
export default function Diagnostics() {
  useThemeRerender();
  const { t } = useTranslation("settings");
  const [report, setReport] = useState<RecentReport | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getDiagnostics()
      .then((text) => {
        if (!cancelled) {
          setReport(recentReport(text));
        }
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          setLoadError((err as Error)?.message || t("diagnostics.loadFailed"));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [t]);

  return (
    <Screen testID="screen-self-diagnostics">
      <Header title={t("diagnostics.title")} backTo={t("mobile:self.title")} />
      <Body contentContainerStyle={{ paddingHorizontal: space.xxl, paddingTop: space.xxl, gap: space.lg }}>
        <Text style={ty.body}>{t("diagnostics.intro")}</Text>
        <Text style={[ty.secondary, { color: semantic.dim }]}>{t("diagnostics.privacy")}</Text>
        <View style={{ flexDirection: "row", gap: space.md }}>
          <View style={{ flex: 1 }}>
            <CopyDiagnosticsButton variant="primary" full short />
          </View>
          <View style={{ flex: 1 }}>
            <SaveDiagnosticsButton />
          </View>
        </View>
        {loadError ? <ErrorText>{loadError}</ErrorText> : null}
        {report && report.omitted > 0 ? (
          <Text testID="text-diagnostics-trimmed" style={[ty.secondary, { color: semantic.dim }]}>
            {t("diagnostics.trimmed")}
          </Text>
        ) : null}
        <View
          style={{
            backgroundColor: semantic.raised,
            borderRadius: r.md,
            padding: space.lg,
          }}
        >
          <Text testID="text-diagnostics-preview" selectable style={[ty.mono, { color: semantic.dim, fontSize: 12 }]}>
            {report?.text ?? t("common:states.loading")}
          </Text>
        </View>
      </Body>
    </Screen>
  );
}
