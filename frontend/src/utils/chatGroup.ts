/**
 * The group whose roles, aliases and moderation apply to the open chat (#2126).
 * Only a channel has one. A DM has no group, so no admins: the selected group
 * stays set while a DM is open (the sidebar keeps it), and must not leak in.
 */
export function chatGroupId(selection: {
  selectedChannelId: string | null;
  selectedGroupId: string | null;
}): string | null {
  if (!selection.selectedChannelId) {
    return null;
  }
  return selection.selectedGroupId;
}
