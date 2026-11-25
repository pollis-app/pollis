// #2109: a file the operator removed is shown as "File removed", once, and never retried. Pure.

/** The core's error text for a removed file (`pollis_core::error::Error::AttachmentRemoved`). */
export const ATTACHMENT_REMOVED = "attachment_removed";

export function isAttachmentRemoved(error: unknown): boolean {
  const message =
    typeof error === "string" ? error : error instanceof Error ? error.message : String(error);
  // Exactly the code, or appended after ": "; a mere mention is not it.
  return message === ATTACHMENT_REMOVED || message.endsWith(`: ${ATTACHMENT_REMOVED}`);
}

/** What a media surface shows when its file cannot be drawn. */
export type MediaProblem = "removed" | "update" | "unavailable";

/** Why `attachment` cannot be drawn, given its resolve `error`; null when nothing is wrong. */
export function mediaProblem(
  attachment: { needs_update?: boolean },
  error: unknown,
): MediaProblem | null {
  if (attachment.needs_update) {
    return "update";
  }
  if (error === null || error === undefined) {
    return null;
  }
  return isAttachmentRemoved(error) ? "removed" : "unavailable";
}
