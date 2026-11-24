/**
 * The label on a message row (#2085): the display name when the author set one, else the handle.
 * Own messages prefer your own display name over the row's (it varies across a send's rows).
 * `null`: nothing to show, so the caller uses its localized fallback. A group alias, when set,
 * wins over all of this (`aliasName`, applied by the caller).
 */
export function authorName(
  senderId: string,
  senderUsername: string | null | undefined,
  self: { id: string; username?: string; preferred_name?: string } | null,
  senderDisplayName?: string | null,
): string | null {
  if (self && senderId === self.id) {
    return self.preferred_name || senderDisplayName || self.username || senderUsername || null;
  }
  return senderDisplayName || senderUsername || null;
}
