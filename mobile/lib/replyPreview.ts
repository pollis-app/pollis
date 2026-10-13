// Quote replies (#1270): what a reply shows of the message it answers, the
// same reference desktop draws (MessageItem's reply line, ReplyPreview's
// bar). Pure, so the node tests can pin it.

export const REPLY_SNIPPET_MAX = 80;

type Quotable = {
  id: string;
  content: string;
  attachments: { filename: string }[];
};

// One line of the replied-to message: its text cut at 80 characters, or, for
// an attachment-only message, the first file's name. Empty when there is
// neither; the caller supplies the placeholder copy.
export function replySnippet(message: Quotable): string {
  const text = message.content.replace(/\s+/g, " ").trim();
  if (text) {
    return text.length > REPLY_SNIPPET_MAX
      ? `${text.slice(0, REPLY_SNIPPET_MAX)}…`
      : text;
  }
  return message.attachments[0]?.filename ?? "";
}

// The loaded messages that something replies to, in ONE pass, keyed by id
// (desktop's #874 shape: no per-row search of the whole log). A reply whose
// target is not loaded is simply absent, and its row says so.
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
