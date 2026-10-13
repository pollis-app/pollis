// How a group's name and description render on its settings page (#1281),
// kept free of react-native so the node tests can pin it. Mirrored by
// frontend/src/utils/identityFields.ts for the desktop rename page; mobile
// imports no frontend TypeScript, so the two are kept in step by hand.
//
// Someone who can edit gets an input, always. Someone who cannot gets the
// value as plain text, and when there is no value at all, nothing: an empty
// box with "What's this group for?" in it reads as an invitation to type that
// the person cannot accept.

export type IdentityFieldMode = "edit" | "read" | "hidden";

export function identityFieldMode(
  canEdit: boolean,
  value: string | null | undefined,
): IdentityFieldMode {
  if (canEdit) {
    return "edit";
  }
  if (value != null && value.trim() !== "") {
    return "read";
  }
  return "hidden";
}

// The viewer's role, preferring the roster (fresh after a role change) and
// falling back to the group row's current_user_role while the roster loads, so
// an admin does not first see the read-only page and then watch it turn into a
// form. Migration 000008 renamed 'owner' to 'admin'; both count.
export function isGroupAdmin(
  rosterRole: string | null | undefined,
  groupRole: string | null | undefined,
): boolean {
  const role = rosterRole ?? groupRole;
  return role === "admin" || role === "owner";
}
