/**
 * The synced preferences blob as stored, keys desktop does not know included (#1650).
 *
 * Desktop reads only the keys it knows into `PreferencesData`, and the blob also holds
 * mobile's (`accent_hex`, `mobile_behavior`, …). A save is therefore merged into the raw
 * blob it was read from, so those keys go back out unchanged.
 */
export type PreferencesBlob = Record<string, unknown>;

/** Parse a stored blob; anything but a JSON object reads as empty. */
export function parsePreferencesBlob(json: string | null | undefined): PreferencesBlob {
  if (!json) {
    return {};
  }
  try {
    const parsed: unknown = JSON.parse(json);
    if (parsed !== null && typeof parsed === "object" && !Array.isArray(parsed)) {
      return parsed as PreferencesBlob;
    }
  } catch {
    // Malformed JSON reads as empty.
  }
  return {};
}

export interface MergedSave {
  /** The blob after the save: what this device now holds. */
  blob: PreferencesBlob;
  /** Keys the save removed from the blob. Sent as `null`, which deletes them on the DS. */
  deleted: Set<string>;
}

/**
 * Merge a save into `blob`. A key set to a value replaces the stored one; a key present
 * with `undefined` or `null` removes it; a key the save does not name is kept.
 *
 * `pendingDeletes` carries removals from earlier saves not sent yet, so a coalesced save
 * still deletes them unless this one sets the key again.
 */
export function mergePreferencesSave(
  blob: PreferencesBlob,
  save: Record<string, unknown>,
  pendingDeletes: ReadonlySet<string> = new Set(),
): MergedSave {
  const next: PreferencesBlob = { ...blob };
  const deleted = new Set(pendingDeletes);
  for (const [key, value] of Object.entries(save)) {
    if (value === undefined || value === null) {
      if (key in next) {
        deleted.add(key);
      }
      delete next[key];
    } else {
      next[key] = value;
      deleted.delete(key);
    }
  }
  return { blob: next, deleted };
}

/** The JSON a save sends: the whole blob, plus a `null` for each key it deletes. */
export function preferencesSaveJson(merged: MergedSave): string {
  const out: Record<string, unknown> = { ...merged.blob };
  for (const key of merged.deleted) {
    out[key] = null;
  }
  return JSON.stringify(out);
}
