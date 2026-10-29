// The pure half of the on-device export (#856), mirroring desktop's ExportArchiveButton.

/// Mirrors `pollis_core::commands::export::MissingAttachment`.
export interface MissingAttachment {
  content_hash: string;
  storage_key: string;
  content_type: string | null;
  file: string;
}

/// Mirrors `pollis_core::commands::export::ExportSummary`.
export interface ExportSummary {
  path: string;
  conversations: number;
  messages: number;
  attachments: number;
  vault_entries: number;
  bytes: number;
  files_dir: string;
  attachments_written: number;
  attachments_missing: MissingAttachment[];
}

/// Mirrors `pollis_core::commands::export_fetch::FetchSummary`.
export interface FetchSummary {
  fetched: number;
  failed: { content_hash: string; file: string; error: string }[];
}

/// The same stems desktop suggests.
export function exportFileName(conversationId: string | null, now: Date = new Date()): string {
  if (conversationId) {
    return `pollis-conversation-${conversationId}.json`;
  }
  return `pollis-archive-${now.toISOString().slice(0, 10)}.json`;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  if (bytes < 1024 * 1024) {
    return `${(bytes / 1024).toFixed(1)} KB`;
  }
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
