import { useTranslation } from "react-i18next";
import { View } from "react-native";
import * as Clipboard from "expo-clipboard";
import { Text } from "../Text";
import { Chip } from "../ui";
import { Icon } from "../icons";
import { AppMark, type AppMarkKind } from "./AppMark";
import { fonts, semantic, type as ty, space } from "../../theme/tokens";
import { haptic } from "../../lib/haptics";
import { openInBrowser } from "../../lib/openLink";
import { isLinkSource, keyFingerprint, type AppManifest } from "../../lib/botApps";

/**
 * Which App this is (#2005): name, version, description, the App hash (or, for a development build,
 * the developer key's fingerprint) to compare with what the developer published, the source as the
 * runtime gave it, and who approved it. Used for the approved App and for the build waiting.
 */
export function AppIdentity({
  manifest,
  appHash,
  developerKey,
  source,
  approvedLine,
  marks = [],
  testID,
}: {
  manifest: AppManifest;
  appHash: string | null;
  developerKey: string | null;
  source: string | null;
  approvedLine?: string;
  marks?: AppMarkKind[];
  testID: string;
}) {
  const { t } = useTranslation("mobile");
  const machine = developerKey ? keyFingerprint(developerKey) : appHash;
  const onCopy = async () => {
    const full = developerKey ?? appHash;
    if (!full) {
      return;
    }
    let copied = false;
    try {
      copied = await Clipboard.setStringAsync(full);
    } catch {
      copied = false;
    }
    haptic(copied ? "success" : "error");
  };
  return (
    <View testID={testID} style={{ gap: space.md }}>
      <View style={{ gap: 2 }}>
        <Text testID={`${testID}-name`} style={[ty.heading, { color: semantic.text }]}>
          {manifest.name}
        </Text>
        <Text style={[ty.secondary, { color: semantic.muted }]}>
          {t("bots.app.version", { version: manifest.version })}
        </Text>
      </View>
      {marks.length > 0 ? (
        <View style={{ flexDirection: "row", flexWrap: "wrap", gap: space.sm }}>
          {marks.map((m) => (
            <AppMark key={m} kind={m} testID={`${testID}-mark-${m}`} />
          ))}
        </View>
      ) : null}
      {manifest.description ? <Text style={ty.secondary}>{manifest.description}</Text> : null}
      {machine ? (
        <View style={{ gap: space.xs }}>
          <Text style={ty.section}>
            {developerKey ? t("bots.app.developerKeyLabel") : t("bots.app.hashLabel")}
          </Text>
          <View style={{ flexDirection: "row", alignItems: "center", gap: space.sm }}>
            <Text
              testID={`${testID}-hash`}
              selectable
              style={[ty.secondary, { flex: 1, fontFamily: fonts.mono400, color: semantic.text }]}
            >
              {machine}
            </Text>
            <Chip
              variant="subtle"
              testID={`${testID}-copy`}
              leading={<Icon.copy size={14} color={semantic.dim} />}
              onPress={onCopy}
            >
              {t("common:actions.copy")}
            </Chip>
          </View>
        </View>
      ) : null}
      {source ? (
        <View style={{ gap: space.xs }}>
          <Text style={ty.section}>{t("bots.app.sourceLabel")}</Text>
          {isLinkSource(source) ? (
            <Text
              testID={`${testID}-source`}
              accessibilityRole="link"
              onPress={() => openInBrowser(source)}
              style={[ty.secondary, { color: semantic.accent, fontFamily: fonts.medium }]}
            >
              {source}
            </Text>
          ) : (
            <Text testID={`${testID}-source`} selectable style={[ty.secondary, { color: semantic.text }]}>
              {source}
            </Text>
          )}
        </View>
      ) : null}
      {approvedLine ? <Text style={[ty.meta, { color: semantic.muted }]}>{approvedLine}</Text> : null}
    </View>
  );
}
