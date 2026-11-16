/**
 * First-run appearance choice (#1924): who is asked to pick Terminal or Modern after sign-in.
 *
 * Asked only when this sign-in is the account's first on this install (a new account, or a
 * device enrolling for the first time) AND the synced preferences carry no skin yet. An account
 * that chose on another device inherits it; an existing install never takes the fresh-sign-in path.
 */

/** What a sign-in result tells us; a subset of `api.AuthResult`. */
export interface SignInShape {
  user: { id: string };
  newSecretKey: string | null;
  enrollmentRequired: boolean;
}

/**
 * A sign-in with no prior local state for the account: a brand-new account (first-device signup
 * hands back a Secret Key) or a device enrolling, by an account this install has not seen before.
 * An install that already knows the account (Forgot PIN recovery, re-sign-in) is not fresh.
 */
export function isFreshSignIn(result: SignInShape, knownAccountIds: readonly string[]): boolean {
  if (knownAccountIds.includes(result.user.id)) {
    return false;
  }
  return result.newSecretKey !== null || result.enrollmentRequired;
}

/** Whether the stored (synced) preferences blob already names a skin. */
export function hasStoredSkin(preferencesJson: string | null | undefined): boolean {
  if (!preferencesJson) {
    return false;
  }
  try {
    const parsed: unknown = JSON.parse(preferencesJson);
    return parsed !== null && typeof parsed === "object" && typeof (parsed as Record<string, unknown>).skin === "string";
  } catch {
    return false;
  }
}

/** Show the first-run appearance step? `preferencesJson` is null when it could not be read: don't ask. */
export function needsAppearanceChoice(freshSignIn: boolean, preferencesJson: string | null): boolean {
  if (!freshSignIn || preferencesJson === null) {
    return false;
  }
  return !hasStoredSkin(preferencesJson);
}
