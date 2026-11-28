// Reads one locale's emoji annotation table from its bundled asset (#2188): the
// `<locale>.emojitable` files under annotations/, which Metro ships as content-addressed assets
// (`assetExts` in metro.config.js), so the Hermes bundle carries none of them and an OTA
// re-sends only a table that changed. The picker's search is all they feed; a read that fails
// or stalls rejects, `useEmojiAnnotations` drops that table and keeps English, and the next
// open tries again. The native modules load on first use, so annotations/index.ts stays
// importable outside React Native (the node tests read EMOJI_ANNOTATION_LOCALES).

import type { EmojiAnnotationRow } from "./annotations/index";

/** How long one table read may take before the picker gives up on it and shows English. */
export const EMOJI_TABLE_READ_TIMEOUT_MS = 5000;

export type EmojiTableRead = (moduleId: number) => Promise<string>;

async function readAssetText(moduleId: number): Promise<string> {
  const [{ Asset }, FileSystem] = await Promise.all([
    import("expo-asset"),
    import("expo-file-system/legacy"),
  ]);
  const asset = Asset.fromModule(moduleId);
  await asset.downloadAsync();
  if (!asset.localUri) {
    throw new Error("emoji table asset has no local copy");
  }
  return FileSystem.readAsStringAsync(asset.localUri);
}

function isRow(value: unknown): value is EmojiAnnotationRow {
  return (
    Array.isArray(value) &&
    value.length === 3 &&
    value.every((part) => typeof part === "string")
  );
}

/** Parse a table's JSON, refusing anything that is not a list of `[char, name, keywords]` rows. */
export function parseEmojiTable(text: string): readonly EmojiAnnotationRow[] {
  const rows: unknown = JSON.parse(text);
  if (!Array.isArray(rows) || !rows.every(isRow)) {
    throw new Error("emoji table is not a list of rows");
  }
  return rows;
}

/**
 * The rows of the asset `source()` names. Rejects, never throws, on any failure (no asset, a
 * failed copy or read, bad JSON) and once `timeoutMs` passes without an answer.
 */
export function readEmojiTable(
  source: () => number,
  read: EmojiTableRead = readAssetText,
  timeoutMs: number = EMOJI_TABLE_READ_TIMEOUT_MS,
): Promise<readonly EmojiAnnotationRow[]> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error("emoji table read timed out")), timeoutMs);
  });
  const rows = (async () => parseEmojiTable(await read(source())))();
  return Promise.race([rows, timeout]).finally(() => {
    if (timer !== undefined) {
      clearTimeout(timer);
    }
  });
}
