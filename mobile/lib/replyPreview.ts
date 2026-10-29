// Quote replies (#1270): what a reply shows of its target, as on desktop. Pure.

export const REPLY_SNIPPET_MAX = 80;

type Quotable = {
  id: string;
  content: string;
  attachments: { filename: string }[];
};

// Text cut at 80 characters, or the first file's name for an attachment-only
// message; empty when neither (the caller supplies the placeholder).
export function replySnippet(message: Quotable): string {
  const text = message.content.replace(/\s+/g, " ").trim();
  if (text) {
    return text.length > REPLY_SNIPPET_MAX
      ? `${text.slice(0, REPLY_SNIPPET_MAX)}…`
      : text;
  }
  return message.attachments[0]?.filename ?? "";
}

// One pass keyed by id (desktop's #874: no per-row search of the log). A target
// that is not loaded is simply absent.
export function replyTargets<M extends Quotable & { reply_to_id?: string | null }>(
  messages: readonly M[],
): Map<string, M> {
  const wanted = new Set<string>();
  for (const m of messages) {
    if (m.reply_to_id) {
      wanted.add(m.reply_to_id);
    }
  }
  const out = new Map<string, M>();
  if (wanted.size === 0) {
    return out;
  }
  for (const m of messages) {
    if (wanted.has(m.id)) {
      out.set(m.id, m);
    }
  }
  return out;
}
