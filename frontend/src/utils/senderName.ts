/**
 * The name a message row shows for its sender (#2085): the display name when they set one, else
 * the handle, else the id. A group alias still wins over this (`aliasName`, at the call site);
 * colours stay keyed on the handle so they do not change with a rename of the display name.
 */
export function senderName(m: {
  sender_id: string;
  sender_username?: string | null;
  sender_display_name?: string | null;
}): string {
  return m.sender_display_name || m.sender_username || m.sender_id;
}

/**
 * What a sender's name colour is keyed on: the handle, else the id. The rule is the same for every
 * sender, the viewer included (#2146), so a name has one colour wherever the chat shows it.
 */
export function senderColorKey(m: { sender_id: string; sender_username?: string | null }): string {
  return m.sender_username ?? m.sender_id;
}
