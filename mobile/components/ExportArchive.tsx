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
import { SectionTitle, ListRow, Chip, Button } from "./ui";
import { Icon } from "./icons";
import { semantic, type as ty } from "../theme/tokens";
import { upper } from "../i18n";
import {
  useExportArchive,
  useFetchExportAttachments,
  useShareExport,
} from "../hooks/queries/useExport";
import { formatBytes } from "../lib/exportArchive";

const noteStyle = {
  fontFamily: ty.body.fontFamily,
  fontSize: 12,
  color: semantic.mute,
  lineHeight: 17,
} as const;

export function ExportArchive({ conversationId = null }: { conversationId?: string | null }) {
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
    <View>
      <SectionTitle>
        {upper(t(conversationId ? "mobile:self.export.conversationHeading" : "security.exportHeading"))}
      </SectionTitle>
      <View style={{ paddingHorizontal: 18, paddingTop: 6, gap: 6 }}>
        <Text style={noteStyle}>{t("security.exportDescription")}</Text>
        <Text style={{ ...noteStyle, color: semantic.mute2 }}>{t("security.exportNote")}</Text>
      </View>
      <ListRow
        testID="row-export-archive"
        minHeight={48}
        glyph={<Icon.download color={semantic.mute} />}
        name={t(conversationId ? "security.exportConversationButton" : "security.exportButton")}
        nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
        sub={exportArchive.isPending ? t("security.exporting") : t("mobile:self.export.rowSub")}
        onPress={() => {
          if (exportArchive.isPending) {
            return;
          }
          setFetched(false);
          fetchAttachments.reset();
          share.reset();
          exportArchive.mutate(conversationId);
        }}
        end={<Icon.fwd color={semantic.mute} />}
      />
      {summary ? (
        <View style={{ paddingHorizontal: 18, paddingTop: 4, gap: 8 }}>
          <Text testID="text-export-summary" style={noteStyle}>
            {t("security.exportDone", {
              messages: t("security.exportMessages", { count: summary.messages }),
              conversations: t("security.exportConversations", { count: summary.conversations }),
              size: formatBytes(summary.bytes),
              path: summary.path,
            })}
          </Text>
          {summary.attachments > 0 ? (
            <Text testID="text-export-files" style={noteStyle}>
              {[
                t("security.exportAttachmentsWritten", { count: summary.attachments_written }),
                t("security.exportAttachmentsMissing", { count: missing }),
              ].join(" · ")}
            </Text>
          ) : null}
          {missing > 0 && !fetched ? (
            <View style={{ gap: 6 }}>
              <Text style={{ ...noteStyle, color: semantic.mute2 }}>{t("security.exportFetchNote")}</Text>
              <View style={{ flexDirection: "row" }}>
                <Chip
                  testID="btn-export-fetch"
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
                </Chip>
              </View>
            </View>
          ) : null}
          {fetchAttachments.data ? (
            <Text testID="text-export-fetched" style={noteStyle}>
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
            icon={<Icon.share color={semantic.ink} />}
            onPress={() => {
              if (summary && !share.isPending) {
                share.mutate(summary);
              }
            }}
            disabled={share.isPending}
          >
            {upper(t(share.isPending ? "mobile:self.export.bundling" : "mobile:self.export.share"))}
          </Button>
        </View>
      ) : null}
      {error ? (
        <Text
          testID="text-export-error"
          style={{ ...noteStyle, color: semantic.danger, paddingHorizontal: 18, paddingTop: 6 }}
        >
          {error}
        </Text>
      ) : null}
    </View>
  );
}
