// How a group's name and description render on its settings page (#1281);
// mirrored by frontend/src/utils/identityFields.ts. A non-editor sees no empty
// field: a box saying "What's this group for?" invites typing they can't do.

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

// Prefers the roster (fresh after a role change), falling back to the group
// row while it loads so an admin never sees the read-only page first.
// Migration 000008 renamed 'owner' to 'admin'; both count.
export function isGroupAdmin(
  rosterRole: string | null | undefined,
  groupRole: string | null | undefined,
): boolean {
  const role = rosterRole ?? groupRole;
  return role === "admin" || role === "owner";
}
