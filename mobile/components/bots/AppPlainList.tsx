import { useTranslation } from "react-i18next";
import { View } from "react-native";
import { Text } from "../Text";
import { Icon } from "../icons";
import { semantic, type as ty, space } from "../../theme/tokens";
import { plainLines, type AdminGrants, type AppManifest, type PlainLine } from "../../lib/botApps";

const GLYPHS: Record<PlainLine["id"], keyof typeof Icon> = {
  reads: "hash",
  events: "messageSquare",
  wakes: "bell",
  can: "send",
  egress: "globe",
  state: "server",
};

/**
 * What an App can do, in plain language, one line per item (#2005). The grants passed in narrow it:
 * for an approved App this is the effective scope, never the raw request.
 */
export function AppPlainList({
  manifest,
  grants,
  testID,
}: {
  manifest: AppManifest;
  grants: AdminGrants;
  testID?: string;
}) {
  const { t } = useTranslation("mobile");
  const lines = plainLines((key, options) => t(key, options) as string, manifest, grants);
  return (
    <View testID={testID} style={{ gap: space.md }}>
      {lines.map((line) => {
        const Glyph = Icon[GLYPHS[line.id]];
        return (
          <View
            key={line.id}
            testID={testID ? `${testID}-${line.id}` : undefined}
            style={{ flexDirection: "row", alignItems: "flex-start", gap: space.md }}
          >
            <View style={{ paddingTop: 2 }}>
              <Glyph size={16} color={semantic.dim} />
            </View>
            <Text style={[ty.secondary, { flex: 1, color: semantic.text }]}>{line.text}</Text>
          </View>
        );
      })}
    </View>
  );
}
