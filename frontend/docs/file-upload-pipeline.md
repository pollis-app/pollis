# File Upload Pipeline Design

> Status: Proposed — not yet implemented.
> Date: 2026-03-27

---

## 1. Goals

1. Optimize images and video before upload (resize, compress, transcode) to conserve R2 storage.
2. Compute a blurhash per file and store it for immediate placeholder rendering.
3. Deduplicate uploads by SHA-256 content hash — identical files are stored once.
4. Evaluate pre-signed URL direct upload vs. the current through-Tauri path and document the chosen approach.
5. Introduce a `files` table in the remote schema as the single authoritative record for uploaded objects.

---

## 2. Current State

### 2.1 Upload path

`r2-upload.ts → invoke('upload_file') → r2.rs::upload_file()` reads the entire file as `Vec<u8>`, computes SigV4 headers in Rust, then issues a `PUT` via `reqwest`. For a 20 MB video the full payload crosses the Tauri IPC bridge twice (JS → Rust serialization as a JSON number array, then a TCP socket write). This is the primary bottleneck.

### 2.2 Existing attachment schema

The **local** SQLite schema (`local_schema.sql`) has:

```sql
CREATE TABLE IF NOT EXISTS attachment (
    id             TEXT PRIMARY KEY,
    message_id     TEXT NOT NULL REFERENCES message(id),
    filename       TEXT NOT NULL,
    mime_type      TEXT NOT NULL,
    size_bytes     INTEGER NOT NULL,
    r2_key         TEXT NOT NULL,
    encryption_key BLOB NOT NULL,
    downloaded     INTEGER NOT NULL DEFAULT 0
);
```

The **remote** `message_envelope` table has no attachment awareness — attachments embedded in ciphertext are opaque to the server.

The frontend `MessageAttachment` interface (`types/index.ts`) has `object_key`, `filename`, `content_type`, `file_size`, `uploaded_at` — no hash, no blurhash, no dimensions.

---

## 3. Database Schema Changes

### 3.1 New `files` table — remote (Turso)

Add to `remote_schema.sql`:

```sql
-- Deduplication registry for uploaded R2 objects.
-- One row per unique file content (keyed by SHA-256 hash).
-- Multiple messages can reference the same file row.
CREATE TABLE files (
    id                  TEXT PRIMARY KEY,       -- ULID
    uploader_id         TEXT NOT NULL REFERENCES users(id) ON DELETE SET NULL,
    r2_key              TEXT NOT NULL UNIQUE,   -- canonical R2 object key
    content_hash        TEXT NOT NULL UNIQUE,   -- hex SHA-256 of original (pre-optimization) bytes
    mime_type           TEXT NOT NULL,
    size_bytes          INTEGER NOT NULL,       -- size after optimization
    original_size_bytes INTEGER NOT NULL,       -- size before optimization
    blurhash            TEXT,                   -- BlurHash string (images/video only)
    width               INTEGER,               -- pixels (images/video)
    height              INTEGER,               -- pixels (images/video)
    duration_secs       INTEGER,               -- video/audio only
    created_at          TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_files_hash     ON files(content_hash);
CREATE INDEX idx_files_uploader ON files(uploader_id);
```

### 3.2 Message-to-file join table — remote (Turso)

```sql
-- Associates one or more files with a message envelope.
-- Ordered by position for display. Using a join table allows the same
-- file to appear in multiple messages (forward feature).
CREATE TABLE message_attachment (
    message_id TEXT NOT NULL REFERENCES message_envelope(id) ON DELETE CASCADE,
    file_id    TEXT NOT NULL REFERENCES files(id) ON DELETE RESTRICT,
    position   INTEGER NOT NULL DEFAULT 0,
    filename   TEXT NOT NULL,   -- original user-visible filename
    PRIMARY KEY (message_id, file_id)
);

CREATE INDEX idx_msg_attachment_message ON message_attachment(message_id, position);
```

### 3.3 Local attachment table — updated

Bump `LOCAL_SCHEMA_VERSION` to `"4"` in `local.rs` and add columns directly to the `CREATE TABLE` statement:

```sql
CREATE TABLE IF NOT EXISTS attachment (
    id             TEXT PRIMARY KEY,
    message_id     TEXT NOT NULL REFERENCES message(id),
    filename       TEXT NOT NULL,
    mime_type      TEXT NOT NULL,
    size_bytes     INTEGER NOT NULL,
    r2_key         TEXT NOT NULL,
    encryption_key BLOB NOT NULL,
    downloaded     INTEGER NOT NULL DEFAULT 0,
    -- New in v4:
    file_id        TEXT,       -- remote files.id (NULL until confirmed)
    content_hash   TEXT,       -- SHA-256 of original bytes
    blurhash       TEXT,
    width          INTEGER,
    height         INTEGER,
    duration_secs  INTEGER
);
```

---

## 4. Architecture Decision: Pre-signed URLs vs. Through-Tauri

### 4.1 Through-Tauri (current)

Rust holds the R2 credentials, signs the request, and proxies the body. Full control — can reject oversized files, wrong MIME types, or modify bytes before writing. The downside: the entire file payload crosses the Tauri IPC bridge as a JSON number array (~1.33× file size in serialized form), synchronously blocking the IPC thread for large payloads.

### 4.2 Pre-signed URL (direct PUT)

Rust generates a time-limited pre-signed PUT URL. The frontend's `fetch()` uploads directly to R2. This eliminates IPC serialization overhead entirely and enables native browser progress events.

**Tradeoff — acknowledged:** Once a pre-signed URL is issued, Rust cannot inspect or intercept the body that reaches R2. The URL must have a short TTL (15 minutes) and an exact `Content-Type` locked at signing time. Content validation (hash verification, blurhash confirmation) still happens after upload by comparing what was stored vs. what was reported.

### 4.3 Chosen approach: pre-signed URL for the PUT body; Rust for everything else

Use pre-signed URLs for the raw upload PUT only. All other steps — optimization, hash computation, blurhash, deduplication check, `files` table write, `message_attachment` insert — run through Tauri commands. This recovers IPC performance for large files while keeping R2 credentials in Rust and all database writes trusted.

Pre-signing is implemented entirely in Rust via SigV4 query-string auth (`X-Amz-Signature` in query params). No Cloudflare Worker is needed.

---

## 5. Rust Command Changes

### 5.1 New command: `prepare_file_upload`

Entry point. Accepts raw file bytes, runs optimization, hashes the result, checks for an existing upload, and either returns a pre-signed URL (new file) or the existing `file_id` (dedup hit).

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct PrepareUploadResult {
    pub upload_url: Option<String>,       // None on dedup hit
    pub r2_key: String,
    pub content_hash: String,             // SHA-256 of original bytes (dedup key)
    pub optimized_hash: String,           // SHA-256 of optimized bytes (integrity)
    pub optimized_data: Option<Vec<u8>>,  // only present when upload_url is Some
    pub optimized_mime: String,
    pub optimized_size: u64,
    pub blurhash: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_secs: Option<u32>,
    pub existing_file_id: Option<String>, // Some on dedup hit
}

#[tauri::command]
pub async fn prepare_file_upload(
    data: Vec<u8>,
    mime_type: String,
    filename: String,
    uploader_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<PrepareUploadResult>
```

Implementation steps:

1. Enforce 100 MB hard cap. Return error if exceeded.
2. Validate `mime_type` against allowlist (see §10.2).
3. `content_hash = sha256_hex(&data)`.
4. Check `files` table: `SELECT id, r2_key, blurhash, width, height, duration_secs FROM files WHERE content_hash = ?`.
5. If found → return `PrepareUploadResult { existing_file_id: Some(id), upload_url: None, ... }`.
6. If not found:
   - `optimize_media(&data, &mime_type)` → `(optimized_data, optimized_mime, width, height, duration_secs)`
   - `optimized_hash = sha256_hex(&optimized_data)`
   - `blurhash = compute_blurhash(...)` (images / first video frame)
   - `r2_key = format!("attachments/{uploader_id}/{YYYY-MM}/{ulid}-{sanitized_filename}")`
   - `upload_url = presign_put_url(r2_key, optimized_mime, ttl=900)`
   - Return all fields.

### 5.2 New command: `confirm_file_upload`

Called after the frontend has successfully PUT to R2. Writes the `files` row.

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct ConfirmUploadParams {
    pub r2_key: String,
    pub content_hash: String,
    pub optimized_hash: String,
    pub mime_type: String,
    pub original_size: u64,
    pub optimized_size: u64,
    pub blurhash: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_secs: Option<u32>,
    pub uploader_id: String,
    pub filename: String,
}

#[tauri::command]
pub async fn confirm_file_upload(
    params: ConfirmUploadParams,
    state: State<'_, Arc<AppState>>,
) -> Result<String>  // returns new files.id (ULID)
```

Uses `INSERT OR IGNORE` + follow-up `SELECT` to handle the race condition where two users upload the same file simultaneously (see §8.3).

### 5.3 Private helper: `presign_put_url`

SigV4 query-string auth, content-type locked at signing time, 15-minute TTL.

```rust
fn presign_put_url(
    endpoint: &str,
    access_key: &str,
    secret_key: &str,
    region: &str,
    key: &str,
    content_type: &str,
    ttl_secs: u64,
) -> Result<String>
```

### 5.4 Private helper: `optimize_media`

```rust
struct OptimizeResult {
    data: Vec<u8>,
    mime_type: String,
    width: Option<u32>,
    height: Option<u32>,
    duration_secs: Option<u32>,
}

fn optimize_media(data: &[u8], mime_type: &str) -> Result<OptimizeResult>
```

See §7 for rules per media type.

### 5.5 Private helper: `compute_blurhash`

```rust
fn compute_blurhash(rgba_pixels: &[u8], width: u32, height: u32) -> String
```

Uses `blurhash = "0.2"` crate with `components_x = 4, components_y = 3`.

### 5.6 `send_message` extension

Add `attachment_file_ids: Vec<String>` parameter. After inserting the envelope, for each `file_id`, insert into `message_attachment`:

```sql
INSERT INTO message_attachment (message_id, file_id, position, filename)
SELECT ?1, f.id, ?2, ?3
FROM files f WHERE f.id = ?4
```

### 5.7 Existing `upload_file` command

Kept as-is for avatars and group icons (small images, no dedup needed). New pipeline is additive.

### 5.8 `lib.rs` additions

```rust
commands::r2::prepare_file_upload,
commands::r2::confirm_file_upload,
```

---

## 6. Blurhash Computation

### 6.1 Where to compute

Computed in Rust inside `prepare_file_upload`, immediately after optimization when pixel data is already in memory.

The `image` crate decodes JPEG/PNG/WEBP/GIF to an RGBA8 pixel buffer. The `blurhash` crate accepts `(pixels: &[u8], width: u32, height: u32)` and returns a `String`. Both are pure Rust, no FFI.

For video: compute from the first decoded frame thumbnail (see §7.2).

### 6.2 Parameters

`components_x = 4, components_y = 3` — standard 4×3 grid, ~30-char string.

### 6.3 Frontend rendering

Use `react-blurhash` npm package. Show the decoded canvas immediately while the real image URL is loading; fade out with a 150 ms CSS transition once the image loads.

---

## 7. Media Optimization

### 7.1 Images

| Input | Output | Max dimension |
|-------|--------|---------------|
| JPEG | JPEG quality 82 | 2048 px longest edge |
| PNG with alpha | WEBP quality 80 (lossless if < 200 KB) | 2048 px |
| PNG without alpha | JPEG quality 82 | 2048 px |
| WEBP | WEBP quality 80 | 2048 px |
| GIF | Pass through (cap 10 MB) | — |
| HEIC/HEIF | JPEG quality 82 via `image` crate HEIF feature | 2048 px |

Images within the dimension limit are not resized. Resize uses Lanczos3. Implemented with the `image` crate.

**Cargo.toml addition:**
```toml
image = { version = "0.25", features = ["jpeg", "png", "webp", "gif"] }
blurhash = "0.2"
```

### 7.2 Video (Phase 1 MVP)

- Hard cap: 50 MB. Reject larger files with a clear error.
- No transcoding in Phase 1.
- Extract first frame as thumbnail via `ffmpeg -i input -vframes 1 -f image2 thumb.jpg` (via `tauri-plugin-shell`). Compute blurhash from thumbnail.
- `duration_secs` from `ffprobe` output.
- Requires `ffmpeg` on PATH or bundled. If unavailable, degrade gracefully: no thumbnail, no duration, no blurhash.

**Phase 2:** Transcode to H.264/AAC MP4 at 1080p max, 4 Mbps video bitrate via `ffmpeg`. Out of scope for initial implementation.

### 7.3 Other types (PDF, archives, etc.)

Pass through. No blurhash. No dimensions. UI shows a generic file icon.

---

## 8. Deduplication

### 8.1 Hash key

`SHA-256(original_bytes_before_optimization)`. Same file uploaded twice → one R2 object. Edited file → new hash → new upload. Correct behavior.

### 8.2 Check location

In Rust inside `prepare_file_upload`, against `files.content_hash` in Turso. The frontend never bypasses this.

### 8.3 Race condition

Two simultaneous uploads of the same file both get a pre-signed URL (both see a miss). The second `confirm_file_upload` hits a UNIQUE constraint on `content_hash`. Handle with:

```sql
INSERT OR IGNORE INTO files (...) VALUES (...);
SELECT id FROM files WHERE content_hash = ?;
```

The duplicate R2 object from the losing upload is an orphan. Options: periodic cleanup Worker, or accept the negligible cost. Decision deferred.

### 8.4 Dedup scope

Currently: global across all users (same hash = same `file_id`, regardless of uploader). Minor privacy implication: User B can detect that an identical file was previously uploaded. Mitigation: scope dedup to `(content_hash, uploader_id)` at the cost of losing cross-user dedup savings. Decision needed before implementation.

---

## 9. Frontend

### 9.1 `useFileUpload` hook

**File:** `frontend/src/hooks/useFileUpload.ts`

```typescript
interface FileUploadState {
  status: 'idle' | 'preparing' | 'uploading' | 'confirming' | 'done' | 'error';
  progress: number;        // 0–100
  fileId: string | null;
  blurhash: string | null;
  width: number | null;
  height: number | null;
  error: string | null;
}

function useFileUpload(): {
  upload: (file: File, uploaderId: string) => Promise<string>; // resolves to file_id
  state: FileUploadState;
  reset: () => void;
}
```

Internal steps in `upload()`:

1. `status = 'preparing'`. Call `invoke('prepare_file_upload', { data, mimeType, filename, uploaderId })`.
2. If `existing_file_id`: `status = 'done'`, return immediately (dedup hit).
3. `status = 'uploading'`. `fetch(upload_url, { method: 'PUT', body: optimizedBlob })`. Use `XMLHttpRequest` for progress events.
4. `status = 'confirming'`. Call `invoke('confirm_file_upload', { ...params })`.
5. `status = 'done'`. Return `file_id`.

### 9.2 `useSendMessageWithAttachments`

Wraps `useSendMessage`. Accepts `files: File[]`, uploads all in parallel (max 3 concurrent), collects `file_id[]`, then calls `send_message` with `attachmentFileIds`. Optimistic UI shows blurhash placeholders immediately.

### 9.3 File picker

New component `components/Message/FilePickerButton.tsx`. Uses `tauri-plugin-dialog` for the native file picker (avoids unreliable `<input type="file">` in WebKitGTK). Returns file paths, read via `tauri-plugin-fs`.

Accepted types at picker level: `image/*`, `video/*`, `application/pdf`, common archives. Enforced in Rust as a secondary check.

### 9.4 Attachment display — blurhash placeholder

In `AttachmentDisplay` (`MessageItem.tsx`):

1. If `attachment.blurhash` is present and the image URL hasn't loaded: render `<Blurhash>` at `width ?? 280` × `height ?? 160`.
2. Once the real image loads: fade out the blurhash canvas with a 150 ms CSS transition.

### 9.5 Updated `MessageAttachment` type

```typescript
export interface MessageAttachment {
  id: string;
  file_id: string;         // remote files.id
  object_key: string;
  filename: string;
  content_type: string;
  file_size: number;
  blurhash?: string;
  width?: number;
  height?: number;
  duration_secs?: number;
  uploaded_at: number;
}
```

### 9.6 `package.json` addition

```
pnpm add react-blurhash
```

---

## 10. Message Query Changes

The `get_channel_messages` and `get_dm_messages` commands return `Vec<ChannelMessage>`. Add `attachments: Vec<AttachmentInfo>` to `ChannelMessage`.

**Fetch strategy:** After fetching the message page, collect the `id[]`, query `message_attachment JOIN files WHERE message_id IN (...)`, assemble on the Rust side. This avoids complex GROUP_CONCAT parsing in the main query.

---

## 11. Security

### 11.1 Pre-signed URL scope

`Content-Type` is locked at signing time. R2 rejects a PUT with a different content type. TTL: 15 minutes.

### 11.2 File size limit

100 MB hard cap enforced in Rust before optimization.

### 11.3 MIME allowlist (Rust)

```
image/jpeg, image/png, image/gif, image/webp, image/heic,
video/mp4, video/quicktime, video/webm,
application/pdf,
application/zip, application/gzip
```

### 11.4 R2 key structure

```
attachments/{uploader_id}/{YYYY-MM}/{ulid}-{sanitized_filename}
```

`sanitized_filename` strips path separators and non-ASCII characters.

### 11.5 Encryption at rest

The local `attachment.encryption_key` column is present but unused. In a future phase, files can be AES-GCM encrypted before upload, keeping the R2 object opaque. Out of scope for this iteration.

---

## 12. Migration Strategy

### 12.1 Remote (Turso)

Add `files` and `message_attachment` to `remote_schema.sql`. Update `push_schema`'s drop list in FK-safe order (`message_attachment` before `files` before `message_envelope`). Run `pnpm db:push`.

### 12.2 Local (SQLite)

Bump `LOCAL_SCHEMA_VERSION` to `"4"`. Add new columns directly to the `CREATE TABLE` statement. The version mismatch wipes and recreates the local DB (acceptable for dev; use `ALTER TABLE ADD COLUMN` gates for production once real users exist).

---

## 13. Open Questions

1. **ffmpeg bundling.** Video thumbnail extraction requires ffmpeg. Bundle (~60 MB addition) or expect it on PATH with graceful degradation?
2. **Dedup scope.** Global across users vs. per-uploader? Global leaks whether an identical file was previously uploaded by another user.
3. **R2 orphan cleanup.** Race-condition duplicates: cleanup Worker on CRON, or accept the cost?
4. **Local file encryption.** `encryption_key` column exists but is unused. Enabling it would make R2 objects opaque ciphertext. Meaningful privacy improvement; deferred.
