import { useEffect, useState } from "react";
import { Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Screen, Header, Body } from "../../components/ui";
import { CopyDiagnosticsButton } from "../../components/self/CopyDiagnosticsButton";
import { ErrorText } from "../../components/self/SettingsField";
import { semantic, type as ty, space, r } from "../../theme/tokens";
import { getDiagnostics } from "../../lib/diagnostics";

/**
 * The on-device diagnostics log (#1305): what it holds, what it never holds,
 * and the exact text "Copy diagnostics" puts on the clipboard — shown in
 * full, so nobody has to take the description on trust. Desktop's
 * `DiagnosticsPage`, same copy.
 */
export default function Diagnostics() {
  const { t } = useTranslation("settings");
  const [report, setReport] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getDiagnostics()
      .then((text) => {
        if (!cancelled) {
          setReport(text);
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
        <CopyDiagnosticsButton variant="primary" full />
        {loadError ? <ErrorText>{loadError}</ErrorText> : null}
        <View
          style={{
            backgroundColor: semantic.raised,
            borderRadius: r.md,
            padding: space.lg,
          }}
        >
          <Text testID="text-diagnostics-preview" selectable style={[ty.mono, { color: semantic.dim, fontSize: 12 }]}>
            {report ?? t("common:states.loading")}
          </Text>
        </View>
      </Body>
    </Screen>
  );
}
