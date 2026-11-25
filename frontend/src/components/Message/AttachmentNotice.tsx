import React from "react";
import { getFileIcon } from "../../utils/fileIcon";

/**
 * An attachment that cannot be opened here, said plainly in place of the
 * file (#2109): one the operator removed ("File removed"), or one from a
 * newer app version ("Update Pollis to open this file"). Nothing to retry.
 */
export function AttachmentNotice({
  filename,
  notice,
  testId,
  kind,
}: {
  filename: string;
  notice: string;
  testId: string;
  kind: "removed" | "update";
}) {
  const FileTypeIcon = getFileIcon(filename);
  return (
    <div
      data-testid={testId}
      data-media-state={kind}
      className="flex items-center gap-2 px-2.5 py-1.5 min-w-0 border-2 border-line bg-surface-high rounded-panel"
      style={{ maxWidth: 360 }}
    >
      <FileTypeIcon size={14} aria-hidden="true" className="text-muted" style={{ flexShrink: 0 }} />
      <span className="text-body font-mono flex-1 min-w-0 truncate text-muted" title={filename}>
        {filename}
      </span>
      <span className="text-secondary flex-shrink-0 text-dim">{notice}</span>
    </div>
  );
}
