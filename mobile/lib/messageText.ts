// The text a draft sends as (#1247): only the ends are trimmed; inner line breaks are content.
export function outgoingText(draft: string): string {
  return draft.trim();
}
