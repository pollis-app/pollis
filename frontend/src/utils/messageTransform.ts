// Wire rows to `Message`s (#2167): pure, so a re-read of an unchanged row deep-equals its cached self.

import type {
  AttachmentKeyScope,
  ContentOrigin,
  LinkPreviewDraft,
  LinkPreviewWire,
  Message,
  MessageAttachment,
  MessageLinkPreview,
} from "../types/index.ts";
import { outboxFields, type SendState } from "./pendingMessages.ts";

// One shared empty buffer: structural sharing treats a fresh typed array as a change. Received rows carry no bytes here.
const NO_BYTES = new Uint8Array();

interface AttachmentWire {
  key: string;
  /** The object id: a random file id (#1382) or a shared `v2-` id (#2109). */
  hash: string;
  /** Which key of the conversation's file key ring sealed a v1 file; `0` in the Vault. */
  kv?: number;
  /** The reference version: `2` for a shared object (#2109); absent for v1. */
  v?: number;
  /** Plaintext SHA-256, verified after decrypt. */
  sha?: string;
  name: string;
  ct: string;
  size: number;
  bh?: string;
  w?: number;
  h?: number;
  /** Sensitive media (#1568); omitted unless set. */
  sens?: boolean;
  /** A video's poster JPEG, base64 (#1837); omitted unless the sender had one. */
  pv?: unknown;
}

export type RawMessage = {
  id: string;
  conversation_id: string;
  sender_id: string;
  content?: string;
  reply_to_id?: string;
  /** ULID of the thread root this was sent into (#825). */
  thread_id?: string;
  sent_at: string;
  /** The sender's link preview (#1987), from the message's own trailer. */
  link_preview?: LinkPreviewWire | null;
};

export type RawChannelMessage = {
  id: string;
  conversation_id: string;
  sender_id: string;
  sender_username?: string;
  sender_display_name?: string | null;
  ciphertext: string;
  content?: string;
  reply_to_id?: string;
  /** ULID of the thread root, recovered from inside the ciphertext (#825). */
  thread_id?: string;
  sent_at: string;
  edited_at?: string;
  deleted_at?: string;
  /** A placeholder for a message this device could not decrypt (#1312). */
  undecryptable?: boolean;
  /** Our own message the durable outbox has not delivered yet (#1335). */
  send_state?: SendState;
  /** Our edit of it the DS has not confirmed yet (#1555); `content` is the edit. */
  edit_state?: SendState;
  /** The sender's link preview (#1987), from the message's own trailer. */
  link_preview?: LinkPreviewWire | null;
};

// Decode the content string: plain text, or `{"_att":[…],"_txt":caption}`. Shared by pins (#99) and the Vault (#107); `origin` names each attachment's key (#1382).
export function parseContent(
  raw: string | undefined,
  origin: ContentOrigin,
): { text: string; attachments: Message['attachments'] } {
  if (!raw?.startsWith('{')) {
    return { text: raw ?? '', attachments: [] };
  }
  try {
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed._att)) {
      return { text: raw, attachments: [] };
    }
    return {
      text: parsed._txt ?? '',
      attachments: (parsed._att as AttachmentWire[]).map((a, i) => attachmentFromWire(a, i, origin)),
    };
  } catch {
    return { text: raw, attachments: [] };
  }
}

/** One `_att` entry as a `MessageAttachment`. No `key` yet: not uploaded by the outbox (#1335), drawn pending. */
function attachmentFromWire(
  a: AttachmentWire,
  i: number,
  origin: ContentOrigin,
): MessageAttachment {
  return {
    id: a.key ?? `uploading-${i}`,
    object_key: a.key ?? '',
    content_hash: a.hash ?? '',
    sha256: a.sha ?? '',
    key_scope: keyScopeFor(a, origin),
    version: a.v === 2 ? 2 : 1,
    ...(isNewerReference(a) ? { needs_update: true } : {}),
    filename: a.name,
    content_type: a.ct,
    file_size: a.size,
    // Not known for a received reference; a constant keeps a re-read row equal to its cached self (#2167).
    uploaded_at: 0,
    blurhash: a.bh,
    width: a.w,
    height: a.h,
    sensitive: a.sens === true,
    poster: posterOf(a.pv),
  };
}

/** A row's `link_preview` (#1987); its thumbnail uses the `_att` field names, so it loads like an image attachment. */
export function linkPreviewFromWire(
  wire: LinkPreviewWire | null | undefined,
  origin: ContentOrigin,
): MessageLinkPreview | null {
  if (!wire || typeof wire.url !== 'string' || typeof wire.title !== 'string' || wire.title === '') {
    return null;
  }
  const image = wire.image;
  return {
    url: wire.url,
    title: wire.title,
    description: typeof wire.description === 'string' && wire.description !== '' ? wire.description : null,
    image:
      image && image.key && typeof image.ct === 'string' && image.ct.startsWith('image/')
        ? attachmentFromWire({ ...image, name: '' }, 0, origin)
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
          object_key: '',
          content_hash: '',
          sha256: '',
          key_scope: null,
          filename: '',
          content_type: 'image/jpeg',
          file_size: 0,
          uploaded_at: Date.now(),
          width: draft.image.width,
          height: draft.image.height,
          localPreviewUrl: draft.image.dataUrl,
        }
      : null,
  };
}

/** Longest `pv` accepted; the core sends at most 16 KiB of it. */
const POSTER_MAX_B64 = 64 * 1024;

/** A sender's `pv` (#1837) only if it is plain base64 of a sane size; anything else is ignored. */
function posterOf(pv: unknown): string | undefined {
  if (typeof pv !== 'string' || pv.length === 0 || pv.length > POSTER_MAX_B64) {
    return undefined;
  }
  return /^[A-Za-z0-9+/]+={0,2}$/.test(pv) ? pv : undefined;
}

/** A reference version this build does not know (#2109): shown as "update to open", never fetched. */
function isNewerReference(a: AttachmentWire): boolean {
  return typeof a.v === 'number' && a.v > 2;
}

/** The attachment's key, or null if it can't be opened (uploading, pre-#1382, or from a newer app). */
function keyScopeFor(a: AttachmentWire, origin: ContentOrigin): AttachmentKeyScope | null {
  if (!a.key || !a.sha || isNewerReference(a)) {
    return null;
  }
  // A shared object (#2109) opens under its own reference, wherever it is.
  if (a.v === 2) {
    return { kind: 'content' };
  }
  if (typeof a.kv !== 'number') {
    return null;
  }
  if (origin === 'vault') {
    return a.kv === 0 ? { kind: 'vault' } : null;
  }
  return a.kv >= 1
    ? { kind: 'conversation', conversation_id: origin.conversationId, key_version: a.kv }
    : null;
}

/** The fields every received message shares; DM and channel rows add their own. */
function baseMessage(m: RawMessage) {
  // Undefined content (decryption failure) stays undefined so MessageList shows [encrypted].
  const parsed = m.content !== undefined ? parseContent(m.content, { conversationId: m.conversation_id }) : null;
  return {
    id: m.id,
    channel_id: undefined,
    conversation_id: m.conversation_id,
    sender_id: m.sender_id,
    ciphertext: NO_BYTES,
    nonce: NO_BYTES,
    content_decrypted: parsed?.text,
    reply_to_message_id: m.reply_to_id,
    thread_id: m.thread_id,
    is_pinned: false,
    created_at: new Date(m.sent_at).getTime(),
    attachments: parsed?.attachments ?? [],
    link_preview: linkPreviewFromWire(m.link_preview, { conversationId: m.conversation_id }),
  };
}

export function transformMessage(m: RawMessage): Message {
  return {
    ...baseMessage(m),
    delivered: true,
    status: 'sent' as const,
  };
}

export function transformChannelMessage(m: RawChannelMessage): Message {
  return {
    ...baseMessage(m),
    sender_username: m.sender_username,
    sender_display_name: m.sender_display_name ?? null,
    edited_at: m.edited_at,
    deleted_at: m.deleted_at,
    undecryptable: m.undecryptable,
    // A pending edit reads as an ordinary edit; only a failed one is labelled.
    edit_unsent: m.edit_state === 'failed' || m.edit_state === 'rejected',
    ...outboxFields(m.id, m.send_state),
  };
}
