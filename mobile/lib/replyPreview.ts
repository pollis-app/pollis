// Quote replies (#1270): what a reply shows of its original. Pure.
import { stripFormatting } from "../../design/messageFormat.ts";

export const REPLY_SNIPPET_MAX = 80;

type Quotable = {
  id: string;
  content: string;
  attachments: { filename: string }[];
};

// Text cut at 80 characters, or the first file's name; empty when neither.
export function replySnippet(message: Quotable): string {
  // The words without the formatting markers (#2142).
  const text = stripFormatting(message.content).replace(/\s+/g, " ").trim();
  if (text) {
    return text.length > REPLY_SNIPPET_MAX
      ? `${text.slice(0, REPLY_SNIPPET_MAX)}…`
      : text;
  }
  return message.attachments[0]?.filename ?? "";
}

// ONE pass, keyed by id (#874); an unloaded target is absent. A reply to a message still
// being sent names the core's id (#2141), which its stub carries as `outboxId`.
export function replyTargets<
  M extends Quotable & { reply_to_id?: string | null; outboxId?: string },
>(
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
    } else if (m.outboxId && wanted.has(m.outboxId)) {
      out.set(m.outboxId, m);
    }
  }
  return out;
}
