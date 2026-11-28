// The message content string, both directions (pure): plain text or the `_att`/`_txt` envelope, in lockstep with desktop's parseContent.

import type {
  AttachmentKeyScope,
  ContentOrigin,
  LinkPreviewDraft,
  LinkPreviewWire,
  MessageAttachment,
  MessageLinkPreview,
} from "../types";

export interface AttachmentWire {
  // Absent on an entry the outbox has not uploaded yet (#1335).
  key?: string;
  url?: string;
  // The object id: a random file id (#1382) or a shared `v2-` id (#2109).
  hash?: string;
  // Key ring version of a v1 file; 0 in the Vault.
  kv?: number;
  // The reference version: 2 for a shared object (#2109); absent for v1.
  v?: number;
  sha?: string;
  name: string;
  ct: string;
  size: number;
  bh?: string;
  w?: number;
  h?: number;
  sens?: boolean;
  // A video's poster JPEG, base64 (#1837); validated by `posterOf`.
  pv?: unknown;
}

/**
 * The `_att` fields that say how a file is keyed: `v: 2` for a shared object
 * (#2109, keyed by its `sha`), else the v1 ring key version `kv`. A v2
 * reference carries no `kv`, so an older app shows it as unavailable instead
 * of trying a ring key. Desktop's `referenceKeyFields`, byte for byte.
 */
export function referenceKeyFields(version: number | undefined, keyVersion: number): { v: 2 } | { kv: number } {
  return version === 2 ? { v: 2 } : { kv: keyVersion };
}

/** Longest `pv` accepted; the core sends at most 16 KiB of it. */
const POSTER_MAX_B64 = 64 * 1024;

/** A sender's `pv` (#1837) only if it is plain base64 of a sane size; anything else is ignored. As desktop's. */
export function posterOf(pv: unknown): string | undefined {
  if (typeof pv !== "string" || pv.length === 0 || pv.length > POSTER_MAX_B64) {
    return undefined;
  }
  return /^[A-Za-z0-9+/]+={0,2}$/.test(pv) ? pv : undefined;
}

/** An image source for a base64 JPEG poster. */
export function posterUri(poster: string): string {
  return `data:image/jpeg;base64,${poster}`;
}

/** A reference version this build does not know (#2109): shown as "update to open", never fetched. As desktop's. */
function isNewerReference(a: AttachmentWire): boolean {
  return typeof a.v === "number" && a.v > 2;
}

/** Null when it cannot be opened from here: uploading, pre-#1382, preview-only, or from a newer app. */
function keyScopeFor(a: AttachmentWire, origin: ContentOrigin): AttachmentKeyScope | null {
  if (origin === null || !a.key || !a.sha || isNewerReference(a)) {
    return null;
  }
  // A shared object (#2109) opens under its own reference, wherever it is.
  if (a.v === 2) {
    return { kind: "content" };
  }
  if (typeof a.kv !== "number") {
    return null;
  }
  if (origin === "vault") {
    return a.kv === 0 ? { kind: "vault" } : null;
  }
  return a.kv >= 1
    ? { kind: "conversation", conversation_id: origin.conversationId, key_version: a.kv }
    : null;
}

/** Split content into caption and attachments; `origin` names each key. */
export function parseContent(raw: string | undefined, origin: ContentOrigin): {
  text: string;
  attachments: MessageAttachment[];
} {
  if (!raw?.startsWith("{")) {
    return { text: raw ?? "", attachments: [] };
  }
  try {
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed._att)) {
      // Legacy `_txt`-only envelopes still reduce to their caption.
      if (typeof parsed?._txt === "string") {
        return { text: parsed._txt, attachments: [] };
      }
      return { text: raw, attachments: [] };
    }
    return {
      text: typeof parsed._txt === "string" ? parsed._txt : "",
      attachments: (parsed._att as AttachmentWire[]).map((a, i) => attachmentFromWire(a, i, origin)),
    };
  } catch {
    return { text: raw, attachments: [] };
  }
}

/** One `_att` entry as a `MessageAttachment`. No `key`: not uploaded by the outbox yet (#1335), drawn pending. */
function attachmentFromWire(a: AttachmentWire, i: number, origin: ContentOrigin): MessageAttachment {
  return {
    id: a.key ?? `uploading-${i}`,
    object_key: a.key ?? "",
    content_hash: a.hash ?? "",
    sha256: a.sha ?? "",
    key_scope: keyScopeFor(a, origin),
    version: a.v === 2 ? 2 : 1,
    ...(isNewerReference(a) ? { needs_update: true } : {}),
    filename: a.name,
    content_type: a.ct,
    file_size: a.size,
    // Not known for a received reference. A constant, so a re-read row deep-equals its cached
    // self and keeps its identity (#2168); a per-read clock value re-rendered every such row.
    uploaded_at: 0,
    blurhash: a.bh,
    width: a.w,
    height: a.h,
    sensitive: a.sens === true,
    poster: posterOf(a.pv),
  };
}

/** A row's `link_preview` (#1987); its thumbnail uses the `_att` field names, so it loads like an image attachment. As desktop's. */
export function linkPreviewFromWire(
  wire: LinkPreviewWire | null | undefined,
  origin: ContentOrigin,
): MessageLinkPreview | null {
  if (!wire || typeof wire.url !== "string" || typeof wire.title !== "string" || wire.title === "") {
    return null;
  }
  const image = wire.image;
  return {
    url: wire.url,
    title: wire.title,
    description:
      typeof wire.description === "string" && wire.description !== "" ? wire.description : null,
    image:
      image && image.key && typeof image.ct === "string" && image.ct.startsWith("image/")
        ? attachmentFromWire({ ...image, name: "" }, 0, origin)
        : null,
  };
}

/** A preview as its own row shows it before the stored row is read back: the composer's thumbnail, local only. */
export function optimisticLinkPreview(
  draft: LinkPreviewDraft | null | undefined,
): MessageLinkPreview | null {
  if (!draft) {
    return null;
  }
  return {
    url: draft.url,
    title: draft.title,
    description: draft.description,
    image: draft.image
      ? {
          id: draft.image.stagedId,
          object_key: "",
          content_hash: "",
          sha256: "",
          key_scope: null,
          filename: "",
          content_type: "image/jpeg",
          file_size: 0,
          // As a received reference's: nothing reads it, and a constant keeps the stub equal.
          uploaded_at: 0,
          width: draft.image.width,
          height: draft.image.height,
          localPreviewUri: draft.image.dataUrl,
        }
      : null,
  };
}
