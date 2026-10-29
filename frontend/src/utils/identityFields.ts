// How a group's or channel's name and description render on the rename page
// (#1281), mirrored by hand in mobile/lib/identityFields.ts. An editor gets an
// input; anyone else gets plain text, or nothing when there is no value.

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
