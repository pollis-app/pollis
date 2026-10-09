// The text a draft sends as (composer, thread composer and edit bar, #1247).
//
// Only the ends are trimmed — the same rule as desktop's ChatInput
// (`message.trim()`). Line breaks INSIDE the draft are content: Return in the
// composer inserts them, they travel over the wire unchanged, and both apps
// render them (desktop with `white-space: pre-wrap`, mobile because RN <Text>
// keeps `\n`). Never collapse or strip them here.
export function outgoingText(draft: string): string {
  return draft.trim();
}
