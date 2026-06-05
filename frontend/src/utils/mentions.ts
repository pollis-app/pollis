// Mirror of `mentions_all()` in pollis-core/src/commands/messages/send.rs.
// Keep the two in sync: the backend is the source of truth for whether a
// message actually pings everyone, and this drives the composer hint that
// tells the sender it will. A standalone `@all` token matches (whitespace-
// delimited, trailing punctuation ignored) so "@all" and "@all," match but
// "@allison" and "email@allcorp" do not. Case-insensitive.
export function mentionsAll(content: string): boolean {
  return content.split(/\s+/).some((word) => {
    // Trim trailing characters that are neither alphanumeric nor '@', the
    // same predicate the Rust matcher uses.
    const trimmed = word.replace(/[^\p{L}\p{N}@]+$/u, "");
    return trimmed.toLowerCase() === "@all";
  });
}
