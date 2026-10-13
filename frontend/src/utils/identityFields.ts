// How a group's or channel's name and description render on the rename page
// (#1281). Mirrored by mobile/lib/identityFields.ts for the mobile group
// settings screen; mobile imports no frontend TypeScript, so the two are kept
// in step by hand.
//
// Someone who can edit gets an input, always. Someone who cannot gets the
// value as plain text, and when there is no value at all, nothing: an empty
// box with a placeholder in it reads as an invitation to type that the person
// cannot accept.

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
