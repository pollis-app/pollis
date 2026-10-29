export interface SnippetPart {
  text: string;
  hit: boolean;
}

/** `[start, end)` UTF-16 ranges; bad ranges are skipped, so the output always reassembles to `text`. */
export function splitSnippet(text: string, highlights: [number, number][]): SnippetPart[] {
  const parts: SnippetPart[] = [];
  let at = 0;
  const ranges = [...highlights].sort((a, b) => a[0] - b[0]);
  for (const [rawStart, rawEnd] of ranges) {
    const start = Math.max(0, rawStart);
    const end = Math.min(text.length, rawEnd);
    if (start < at || end <= start) {
      continue;
    }
    if (start > at) {
      parts.push({ text: text.slice(at, start), hit: false });
    }
    parts.push({ text: text.slice(start, end), hit: true });
    at = end;
  }
  if (at < text.length) {
    parts.push({ text: text.slice(at), hit: false });
  }
  return parts;
}
