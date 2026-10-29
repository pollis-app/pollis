// On-device export (#856): desktop's commands plus `bundle_export`, handed to the share sheet as one zip.

import { useMutation } from "@tanstack/react-query";
import * as FileSystem from "expo-file-system/legacy";
import * as Sharing from "expo-sharing";
import { invoke } from "../../lib/native";
import i18n from "../../i18n";
import { EXPORT_DIR } from "../../lib/exportDir";
import {
  exportFileName,
  type ExportSummary,
  type FetchSummary,
} from "../../lib/exportArchive";

export type { ExportSummary, FetchSummary, MissingAttachment } from "../../lib/exportArchive";

export { EXPORT_DIR } from "../../lib/exportDir";

export function useExportArchive() {
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
    mutationFn: async (conversationId: string | null): Promise<ExportSummary> => {
      await FileSystem.makeDirectoryAsync(EXPORT_DIR, { intermediates: true });
      const path = `${EXPORT_DIR}${exportFileName(conversationId)}`;
      return invoke<ExportSummary>("export_archive", { path, conversationId });
    },
  });
}

export function useFetchExportAttachments() {
  return useMutation({
    mutationFn: async (summary: ExportSummary): Promise<FetchSummary> =>
      invoke<FetchSummary>("fetch_export_attachments", {
        filesDir: summary.files_dir,
        attachments: summary.attachments_missing,
      }),
  });
}

export function useShareExport() {
  return useMutation({
    mutationFn: async (summary: ExportSummary): Promise<void> => {
      const zip = await invoke<string>("bundle_export", { path: summary.path });
      if (!(await Sharing.isAvailableAsync())) {
        throw new Error(i18n.t("mobile:self.export.sharingUnavailable"));
      }
      await Sharing.shareAsync(zip, {
        mimeType: "application/zip",
        UTI: "public.zip-archive",
        dialogTitle: i18n.t("mobile:self.export.shareTitle"),
      });
    },
  });
}
