// Sharing into Pollis from another app (#2100): the picker's pure rules. Import-free, so node
// tests drive them directly (tests/share-picker.test.ts).
//
// The same rules as the iOS share extension's Swift (plugins/share-extension/): up to 5 chats,
// up to 10 items of at most 100 MB each, and the shared text or link pre-fills the message.

/** Most chats one share goes to (pollis-core `share::targets::MAX_TARGETS`). */
export const MAX_TARGETS = 5;
/** Most files one share carries (pollis-core `share::send::MAX_ITEMS`). */
export const MAX_ITEMS = 10;
/** The attachment cap (`R2_MEDIA_MAX_BYTES`). */
export const MAX_BYTES = 100 * 1024 * 1024;

export type ItemKind = "image" | "video" | "file";

/** One file the intake copied. */
interface SharedItem {
  path: string;
  name: string;
  mimeType: string;
  size: number;
}

/** What the Android intake wrote (`ShareInbox.kt`'s `share.json`). */
interface ShareManifest {
  id: string;
  createdAt: number;
  text?: string;
  subject?: string;
  items: SharedItem[];
  refused: { name: string; mimeType: string; reason: "too_large" | "unreadable" }[];
  truncated: boolean;
}

/** A share as the picker draws it. */
export interface ShareDraft {
  id: string;
  /** What the message field starts with. */
  message: string;
  items: (SharedItem & { kind: ItemKind })[];
  /** Names of files left out for being over the cap. */
  tooLarge: string[];
  /** How many shared items the intake could not read (no access, or gone). */
  unreadable: number;
  /** More than [MAX_ITEMS] were shared; the rest were left out. */
  truncated: boolean;
}

export function itemKind(mimeType: string): ItemKind {
  if (mimeType.startsWith("image/")) {
    return "image";
  }
  if (mimeType.startsWith("video/")) {
    return "video";
  }
  return "file";
}

/**
 * The message a share starts with. Browsers send the page title as the subject and the link as
 * the text: the link alone is what a person pastes, so the subject is used only when there is no
 * text at all.
 */
export function initialMessage(text: string | undefined, subject: string | undefined): string {
  const body = (text ?? "").trim();
  if (body) {
    return body;
  }
  return (subject ?? "").trim();
}

/** Parse the intake's manifest; null when it is not one. */
export function parseManifest(json: string | null | undefined): ShareDraft | null {
  if (!json) {
    return null;
  }
  let raw: unknown;
  try {
    raw = JSON.parse(json);
  } catch {
    return null;
  }
  const m = raw as Partial<ShareManifest>;
  if (typeof m?.id !== "string" || !m.id) {
    return null;
  }
  const items = (Array.isArray(m.items) ? m.items : [])
    .filter(
      (i): i is SharedItem =>
        !!i && typeof i.path === "string" && typeof i.name === "string" && typeof i.mimeType === "string",
    )
    .filter((i) => (i.size ?? 0) <= MAX_BYTES)
    .slice(0, MAX_ITEMS)
    .map((i) => ({ ...i, size: i.size ?? 0, kind: itemKind(i.mimeType) }));
  const tooLarge = (Array.isArray(m.refused) ? m.refused : [])
    .filter((r) => r?.reason === "too_large" && typeof r.name === "string")
    .map((r) => r.name);
  const unreadable = (Array.isArray(m.refused) ? m.refused : []).filter(
    (r) => r?.reason === "unreadable",
  ).length;
  return {
    id: m.id,
    message: initialMessage(m.text, m.subject),
    items,
    tooLarge,
    unreadable,
    truncated: m.truncated === true || (Array.isArray(m.items) && m.items.length > MAX_ITEMS),
  };
}

/** Select or unselect `id`. A sixth choice is refused (`capped`) and changes nothing. */
export function toggleTarget(
  selected: readonly string[],
  id: string,
  max: number = MAX_TARGETS,
): { selected: string[]; capped: boolean } {
  if (selected.includes(id)) {
    return { selected: selected.filter((s) => s !== id), capped: false };
  }
  if (selected.length >= max) {
    return { selected: [...selected], capped: true };
  }
  return { selected: [...selected, id], capped: false };
}

/** Whether Send may be pressed: at least one chat, and something to send. */
export function canSend(selected: readonly string[], message: string, draft: ShareDraft | null): boolean {
  if (selected.length === 0 || selected.length > MAX_TARGETS) {
    return false;
  }
  return message.trim().length > 0 || (draft?.items.length ?? 0) > 0;
}

/** Where the app goes once a share is stored: the chat itself for one, back for several. */
export function afterSend(targets: readonly { conversationId: string; kind: "dm" | "channel" | "vault" }[]):
  | { to: "chat"; conversationId: string; kind: "dm" | "channel" }
  | { to: "vault" }
  | { to: "back" } {
  const only = targets.length === 1 ? targets[0] : null;
  if (only?.kind === "vault") {
    return { to: "vault" };
  }
  if (only) {
    return { to: "chat", conversationId: only.conversationId, kind: only.kind === "dm" ? "dm" : "channel" };
  }
  return { to: "back" };
}

/** The request `share_enqueue` takes (pollis-core `share::send::ShareRequest`). */
export function shareRequest(
  shareId: string,
  targets: readonly string[],
  message: string,
  draft: ShareDraft | null,
  // A video's frame from the platform decoder, by path (#1837): the core makes its poster.
  posters: Readonly<Record<string, string>> = {},
): {
  shareId: string;
  targets: string[];
  text: string;
  attachments: { path: string; filename: string; contentType: string; poster?: string }[];
} {
  return {
    shareId,
    targets: [...targets],
    text: message.trim(),
    attachments: (draft?.items ?? []).map((i) => ({
      path: i.path,
      filename: i.name,
      contentType: i.mimeType,
      ...(posters[i.path] ? { poster: posters[i.path] } : {}),
    })),
  };
}
