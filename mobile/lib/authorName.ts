/**
 * The name shown on a message row. Your own rows never use `sender_username`:
 * the optimistic stub, the send result and a refetch disagree on it, so the
 * label flipped when a send reconciled. `null` means show "unknown author".
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
