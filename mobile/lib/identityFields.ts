// How a group's name and description render on its settings page (#1281); mirrored by desktop's identityFields.ts. Pure.

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

// Roster first (fresh after a role change), then the group row; 'owner' (pre-000008) counts too.
export function isGroupAdmin(
  rosterRole: string | null | undefined,
  groupRole: string | null | undefined,
): boolean {
  const role = rosterRole ?? groupRole;
  return role === "admin" || role === "owner";
}
