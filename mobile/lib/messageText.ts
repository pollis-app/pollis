// The text a draft sends as (#1247): only the ends are trimmed, as on desktop.
// Line breaks inside the draft are content; never collapse or strip them.
export function outgoingText(draft: string): string {
  return draft.trim();
}
