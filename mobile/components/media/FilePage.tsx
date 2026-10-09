// The viewer page for an attachment that is not image, video or audio
// (#1248): its name and size, and a pointer to Share — the share sheet is
// how a phone opens a file in another app (desktop saves it to disk).

import { Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { formatBytes } from "../../lib/exportArchive";
import { fonts, semantic, space, type as ty } from "../../theme/tokens";
import type { MessageAttachment } from "../../types";

export function FilePage({
  attachment,
  width,
  height,
  testID,
}: {
  attachment: MessageAttachment;
  width: number;
  height: number;
  testID?: string;
}) {
  const { t } = useTranslation("mobile");
  return (
    <View
      testID={testID}
      style={{
        width,
        height,
        alignItems: "center",
        justifyContent: "center",
        gap: space.md,
        padding: space.xxl,
      }}
    >
      <Icon.file size={40} color={semantic.dim} />
      <Text
        numberOfLines={3}
        style={[ty.body, { fontFamily: fonts.medium, color: semantic.text, textAlign: "center" }]}
      >
        {attachment.filename}
      </Text>
      {attachment.file_size > 0 ? (
        <Text style={ty.meta}>{formatBytes(attachment.file_size)}</Text>
      ) : null}
      <Text style={[ty.secondary, { color: semantic.dim, textAlign: "center" }]}>
        {t("media.fileHint")}
      </Text>
    </View>
  );
}
