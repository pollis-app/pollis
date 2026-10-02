/**
 * The name shown on a message row.
 *
 * Your own messages always show your display name (`preferred_name`), falling
 * back to your username. They never use the row's `sender_username`: the
 * optimistic stub, the row `send_message` returns and the row a later refetch
 * delivers disagree about that field, which made the label flip from "you" to
 * the username when a send reconciled. Everyone else's rows show the sender's
 * username. `null` means there is nothing to show, and the caller substitutes
 * its localized "unknown author" label.
 */
export function authorName(
  senderId: string,
  senderUsername: string | undefined,
  self: { id: string; username?: string; preferred_name?: string } | null,
): string | null {
  if (self && senderId === self.id) {
    return self.preferred_name || self.username || senderUsername || null;
  }
  return senderUsername || null;
}
