/**
 * Release notes for the Update page (#1916): the changelog entry's Markdown-style text, parsed
 * into blocks the page renders as React text nodes. The text arrives from the CDN's updater feed,
 * so it is never HTML and never a link: anything this does not recognise stays literal text.
 */

export type NoteSpan = { text: string; style: "plain" | "bold" | "code" };

export type NoteBlock =
  | { kind: "heading"; spans: NoteSpan[] }
  | { kind: "paragraph"; spans: NoteSpan[] }
  | { kind: "list"; items: NoteSpan[][] };

const BULLET = /^\s*[-*]\s+(.*)$/;
const HEADING = /^#{1,6}\s+(.*)$/;
// **bold**, `code`, or an <https://…> autolink (shown as its bare address).
const INLINE = /\*\*([^*]+)\*\*|`([^`]+)`|<(https?:\/\/[^\s<>]+)>/g;

/** One line of text split into plain, bold and code runs. */
export function parseNoteSpans(text: string): NoteSpan[] {
  const spans: NoteSpan[] = [];
  const plain = (value: string) => {
    if (!value) {
      return;
    }
    const last = spans[spans.length - 1];
    if (last && last.style === "plain") {
      last.text += value;
    } else {
      spans.push({ text: value, style: "plain" });
    }
  };
  let at = 0;
  for (const match of text.matchAll(INLINE)) {
    plain(text.slice(at, match.index));
    if (match[1] !== undefined) {
      spans.push({ text: match[1], style: "bold" });
    } else if (match[2] !== undefined) {
      spans.push({ text: match[2], style: "code" });
    } else {
      plain(match[3]);
    }
    at = match.index + match[0].length;
  }
  plain(text.slice(at));
  return spans;
}

/** The notes as headings, paragraphs and bullet lists; a wrapped line joins the one above it. */
export function parseReleaseNotes(notes: string): NoteBlock[] {
  // Raw text per block; spans are cut once the wrapped lines are joined.
  type Draft =
    | { kind: "heading"; text: string }
    | { kind: "paragraph"; text: string }
    | { kind: "list"; items: string[] };
  const drafts: Draft[] = [];
  // False after a blank line or a heading: the next plain line starts a new paragraph.
  let open = false;

  for (const raw of notes.replace(/\r\n?/g, "\n").split("\n")) {
    const line = raw.trimEnd();
    if (!line.trim()) {
      open = false;
      continue;
    }
    const last = drafts[drafts.length - 1];
    const bullet = line.match(BULLET);
    if (bullet) {
      // Bullets separated by a blank line are still one list.
      if (last && last.kind === "list") {
        last.items.push(bullet[1]);
      } else {
        drafts.push({ kind: "list", items: [bullet[1]] });
      }
      open = true;
      continue;
    }
    const heading = line.match(HEADING);
    if (heading) {
      drafts.push({ kind: "heading", text: heading[1] });
      open = false;
      continue;
    }
    const text = line.trim();
    if (open && last && last.kind === "list") {
      last.items[last.items.length - 1] += ` ${text}`;
    } else if (open && last && last.kind === "paragraph") {
      last.text += ` ${text}`;
    } else {
      drafts.push({ kind: "paragraph", text });
      open = true;
    }
  }

  return drafts.map((draft): NoteBlock => {
    if (draft.kind === "list") {
      return { kind: "list", items: draft.items.map(parseNoteSpans) };
    }
    return { kind: draft.kind, spans: parseNoteSpans(draft.text) };
  });
}
