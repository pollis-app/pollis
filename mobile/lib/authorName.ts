/** Own messages show your display name, never the row's `sender_username` (it varies across a send's rows). `null`: nothing to show. */
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
