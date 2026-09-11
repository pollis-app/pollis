// On-device export (#856) — the mobile counterpart of desktop's
// `ExportArchiveButton`. Same three Rust commands, plus `bundle_export`:
// a sandboxed app has no "save as", so the archive is written under the
// app's cache dir and handed to the OS share sheet as one zip.
//
// Only `useFetchExportAttachments` touches the network, and only when the
// user presses its own button after the archive is already on disk.

import { useMutation } from "@tanstack/react-query";
import * as FileSystem from "expo-file-system/legacy";
import * as Sharing from "expo-sharing";
import { invoke } from "../../lib/native";
import i18n from "../../i18n";
import {
  exportFileName,
  type ExportSummary,
  type FetchSummary,
} from "../../lib/exportArchive";

export type { ExportSummary, FetchSummary, MissingAttachment } from "../../lib/exportArchive";

/// Under `cacheDirectory` on purpose: the OS may evict it, and the share
/// sheet has already copied the zip wherever the user sent it.
export const EXPORT_DIR = `${FileSystem.cacheDirectory ?? ""}pollis-export/`;

export function useExportArchive() {
  return useMutation({
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
