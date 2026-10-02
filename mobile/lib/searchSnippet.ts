// Pure snippet splitting for search hits, free of react-native so `node
// --test` can load it.

export interface SnippetPart {
  text: string;
  hit: boolean;
}

/**
 * Split a snippet into plain and highlighted runs. `highlights` are
 * `[start, end)` pairs in UTF-16 code units (plain JS string indices). Ranges
 * are taken in order; an empty, inverted or overlapping range is skipped
 * rather than trusted, so the output always reassembles to `text` exactly.
 */
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
