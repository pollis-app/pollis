// "Your data" (#856): export what this phone holds, optionally complete the
// attachments, then hand the zip to the share sheet. One component so the
// Security screen (account) and the two conversation-info screens
// (conversation) render the identical flow.
//
// Copy comes from the SAME `settings:security.export*` keys desktop renders;
// only the share-sheet step, which desktop does not have, lives under
// `mobile:self.export`.
import { useState } from "react";
import { View, Text } from "react-native";
import { useTranslation } from "react-i18next";
import { Group, ListRow, Button, SectionTitle } from "./ui";
import { Icon } from "./icons";
import { ErrorText } from "./self/SettingsField";
import { semantic, type as ty, space } from "../theme/tokens";
import {
  useExportArchive,
  useFetchExportAttachments,
  useShareExport,
} from "../hooks/queries/useExport";
import { formatBytes } from "../lib/exportArchive";

export function ExportArchive({
  conversationId = null,
  padded = true,
}: {
  conversationId?: string | null;
  // Adds the 16pt side gutter. Pass false when the parent already pads.
  padded?: boolean;
}) {
  const { t } = useTranslation("settings");
  const exportArchive = useExportArchive();
  const fetchAttachments = useFetchExportAttachments();
  const share = useShareExport();
  const [fetched, setFetched] = useState(false);

  const summary = exportArchive.data ?? null;
  const missing = summary?.attachments_missing.length ?? 0;
  const error =
    (exportArchive.error as Error | null)?.message ??
    (fetchAttachments.error as Error | null)?.message ??
    (share.error as Error | null)?.message ??
    null;

  return (
    <View style={{ gap: space.md, paddingHorizontal: padded ? space.xxl : 0 }}>
      <SectionTitle style={{ paddingHorizontal: 4, paddingTop: 0, paddingBottom: 0 }}>
        {t(conversationId ? "mobile:self.export.conversationHeading" : "security.exportHeading")}
      </SectionTitle>
      <Text style={[ty.secondary, { paddingHorizontal: 4 }]}>{t("security.exportDescription")}</Text>
      <Group>
        <ListRow
          testID="row-export-archive"
          glyph={<Icon.download size={22} color={semantic.text} />}
          name={t(conversationId ? "security.exportConversationButton" : "security.exportButton")}
          sub={exportArchive.isPending ? t("security.exporting") : t("mobile:self.export.rowSub")}
          chevron
          onPress={() => {
            if (exportArchive.isPending) {
              return;
            }
            setFetched(false);
            fetchAttachments.reset();
            share.reset();
            exportArchive.mutate(conversationId);
          }}
        />
      </Group>
      {summary ? (
        <View style={{ gap: space.md, paddingHorizontal: 4 }}>
          <Text testID="text-export-summary" style={ty.secondary}>
            {t("security.exportDone", {
              messages: t("security.exportMessages", { count: summary.messages }),
              conversations: t("security.exportConversations", { count: summary.conversations }),
              size: formatBytes(summary.bytes),
              path: summary.path,
            })}
          </Text>
          {summary.attachments > 0 ? (
            <Text testID="text-export-files" style={ty.secondary}>
              {[
                t("security.exportAttachmentsWritten", { count: summary.attachments_written }),
                t("security.exportAttachmentsMissing", { count: missing }),
              ].join(" · ")}
            </Text>
          ) : null}
          {missing > 0 && !fetched ? (
            <View style={{ gap: space.sm, alignItems: "flex-start" }}>
              <Text style={ty.meta}>{t("security.exportFetchNote")}</Text>
              <Button
                testID="btn-export-fetch"
                icon={<Icon.download size={18} color={semantic.text} />}
                accessibilityLabel={t("security.exportFetchButton", { count: missing })}
                onPress={() => {
                  if (!summary || fetchAttachments.isPending) {
                    return;
                  }
                  fetchAttachments.mutate(summary, { onSuccess: () => setFetched(true) });
                }}
              >
                {fetchAttachments.isPending
                  ? t("security.exportFetching")
                  : t("security.exportFetchButton", { count: missing })}
              </Button>
            </View>
          ) : null}
          {fetchAttachments.data ? (
            <Text testID="text-export-fetched" style={ty.secondary}>
              {[
                t("security.exportFetched", { count: fetchAttachments.data.fetched }),
                fetchAttachments.data.failed.length > 0
                  ? t("security.exportFetchFailed", { count: fetchAttachments.data.failed.length })
                  : null,
              ]
                .filter(Boolean)
                .join(" · ")}
            </Text>
          ) : null}
          <Button
            full
            testID="btn-export-share"
            variant="primary"
            icon={<Icon.share size={18} color={semantic.onAccent} />}
            onPress={() => {
              if (summary && !share.isPending) {
                share.mutate(summary);
              }
            }}
            disabled={share.isPending}
          >
            {t(share.isPending ? "mobile:self.export.bundling" : "mobile:self.export.share")}
          </Button>
        </View>
      ) : null}
      {error ? (
        <View style={{ paddingHorizontal: 4 }}>
          <ErrorText testID="text-export-error">{error}</ErrorText>
        </View>
      ) : null}
    </View>
  );
}
