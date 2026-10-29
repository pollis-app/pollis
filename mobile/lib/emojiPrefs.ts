// Device-local emoji picker prefs (skin tone, recents) in a sandbox JSON file; a corrupt file yields defaults.

import * as FileSystem from "expo-file-system/legacy";

const PREFS_URI = `${FileSystem.documentDirectory ?? ""}emoji-prefs.json`;
const RECENTS_MAX = 40;

interface EmojiPrefs {
  tone: number;
  recents: string[];
}

const prefs: EmojiPrefs = { tone: 0, recents: [] };
let hydrated = false;

function sanitizeTone(value: unknown): number {
  const n = typeof value === "number" ? value : Number.NaN;
  if (!Number.isFinite(n) || n < 0 || n > 5) {
    return 0;
  }
  return Math.floor(n);
}

export async function hydrateEmojiPrefs(): Promise<void> {
  if (hydrated) {
    return;
  }
  hydrated = true;
  try {
    const info = await FileSystem.getInfoAsync(PREFS_URI);
    if (!info.exists) {
      return;
    }
    const raw = await FileSystem.readAsStringAsync(PREFS_URI);
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) {
      return;
    }
    const obj = parsed as Record<string, unknown>;
    prefs.tone = sanitizeTone(obj.tone);
    if (Array.isArray(obj.recents)) {
      prefs.recents = obj.recents
        .filter((v): v is string => typeof v === "string")
        .slice(0, RECENTS_MAX);
    }
  } catch {
    // Defaults stand.
  }
}

function persist(): void {
  // Fire-and-forget: a lost write costs only the recents.
  void FileSystem.writeAsStringAsync(PREFS_URI, JSON.stringify(prefs)).catch(
    () => {},
  );
}

export function readSkinTone(): number {
  return prefs.tone;
}

export function writeSkinTone(toneIndex: number): void {
  prefs.tone = sanitizeTone(toneIndex);
  persist();
}

export function readRecentEmojiIds(): string[] {
  return prefs.recents.slice();
}

export function recordRecentEmojiId(id: string): string[] {
  prefs.recents = [id, ...prefs.recents.filter((v) => v !== id)].slice(
    0,
    RECENTS_MAX,
  );
  persist();
  return prefs.recents.slice();
}
