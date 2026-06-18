# Run Pollis yourself

Pollis is not "open source" by license, but it is **mechanically open**: the
repository is public, the desktop client *is* the backend (there is no
proprietary server in the middle), and nothing stops you from pointing the app
at infrastructure you own and running the whole thing end to end.

This guide is the honest version of "don't trust us — check." You provision your
own database, your own media server, and your own file storage, build the real
client from this source tree, and watch it work. Because the security model
assumes the server is hostile (see [security-explained.md](security-explained.md)
and [security-whitepaper.md](security-whitepaper.md)), running *your own* server
is exactly the adversarial position: if the operator could read messages or hear
audio, you — now the operator — would be able to, and you can confirm you can't.

> This builds the **Tauri** desktop app (the shipping shell). The `electron/`
> directory is deprecated legacy — see [../electron/DEPRECATED.md](../electron/DEPRECATED.md).

---

## What you'll stand up

| Piece | Role | Self-host option |
|---|---|---|
| **Turso (libSQL)** | Stores encrypted message envelopes + group/channel metadata. Never sees plaintext. | Turso Cloud free tier, or self-hosted `sqld` |
| **LiveKit SFU** | Routes voice/screenshare RTP. Forwards ciphertext frames it cannot decode. | Self-host via [`livekit/`](../livekit/), or LiveKit Cloud |
| **Cloudflare R2** | Stores encrypted file/image blobs. | Any S3-compatible bucket |
| **Resend** | Delivers the email OTP sign-in code. | Optional — bypass with `DEV_OTP` |

The point of running all four yourself: every byte these services hold is
ciphertext, and the keys that open it never leave the device. You can inspect
the Turso rows, record the LiveKit stream server-side, and read the R2 objects —
and confirm none of it is intelligible without the client-held MLS keys.

---

## 1. Prerequisites

- **Rust** (stable, via rustup) and a C/C++ toolchain
- **Node.js** 18+ and **pnpm** 10.25+
- **Tauri system dependencies** for your OS — follow
  <https://v2.tauri.app/start/prerequisites/>. On Debian/Ubuntu that's roughly:
  ```bash
  sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
    libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
  ```

```bash
git clone https://github.com/actuallydan/pollis.git
cd pollis
pnpm install
```

## 2. Provision a Turso database

Create a database and grab its URL + auth token (Turso Cloud free tier is fine):

```bash
turso db create pollis-selfhost
turso db show pollis-selfhost --url        # -> libsql://...turso.io
turso db tokens create pollis-selfhost     # -> the auth token
```

The schema is created/migrated by the client on first connect — no manual SQL.

## 3. Stand up your own LiveKit SFU

This is the part that matters most for the voice E2E claim, because you become
the SFU operator. The reference config lives in [`livekit/`](../livekit/) — full
walkthrough in [`livekit/DEPLOY.md`](../livekit/DEPLOY.md). The short version:

```bash
# On a server with a domain + TLS cert:
livekit-server generate-keys            # -> API key + secret
# put them in livekit/livekit.yml under `keys:`, set your domain in nginx.conf
docker compose -f livekit/docker-compose.yml up -d
```

Note the `wss://` URL and the API key/secret — they go in your `.env` next. For a
purely local sanity check you can also run `livekit-server --dev` and use its
well-known dev key/secret, but TURN/TLS won't be set up.

## 4. Create an R2 bucket (or any S3-compatible store)

From the Cloudflare dashboard: create an R2 bucket, generate an S3 access
key/secret, and note the S3 endpoint and a public URL for serving objects.

## 5. Configure credentials

Copy the sample and fill in everything you provisioned:

```bash
cp .env.example .env.development
```

```ini
TURSO_URL=libsql://pollis-selfhost-....turso.io
TURSO_TOKEN=<token from step 2>

R2_ACCESS_KEY_ID=<from step 4>
R2_SECRET_KEY=<from step 4>
R2_S3_ENDPOINT=https://<account>.r2.cloudflarestorage.com/<bucket>
R2_PUBLIC_URL=https://<your-public-r2-url>

LIVEKIT_URL=wss://<your-livekit-domain>
LIVEKIT_API_KEY=<from step 3>
LIVEKIT_API_SECRET=<from step 3>

# Resend is required to build Config; a placeholder is fine if you bypass email:
RESEND_API_KEY=placeholder
DEV_OTP=000000          # skip the email send; type 000000 as the OTP code
```

(See [`pollis-core/src/config.rs`](../pollis-core/src/config.rs) for the exact
variables the core reads.)

## 6. Run it

```bash
pnpm dev
```

First build compiles the Rust crates and can take a few minutes. Sign in with
your email and the `DEV_OTP` code; on first sign-up the app shows your Secret Key
**once** — save it.

### Two users on one machine

`POLLIS_DATA_DIR` gives a second instance its own keystore + local DB:

```bash
# Terminal 1
pnpm dev
# Terminal 2
POLLIS_DATA_DIR=/tmp/pollis-2 DEV_EMAIL=other@example.com pnpm dev
```

Both hit your Turso DB and your LiveKit SFU, so you can create a group, exchange
messages, and start a voice call between them.

## 7. Build a real installer

```bash
pnpm build:tauri          # current platform; outputs per src-tauri/tauri.conf.json
```

---

## What to actually verify

Running it is the setup; the proof is in what your own infrastructure *can't* do.

1. **The database holds ciphertext.** Open your Turso DB and read the message
   envelope rows directly (`turso db shell pollis-selfhost`). You'll see opaque
   ciphertext and metadata (who/when/how big), never message text — the
   "envelope, not contents" model.

2. **The SFU forwards ciphertext it can't decode.** Start a voice call between
   your two clients, then capture what your LiveKit server is forwarding —
   server-side track egress/recording, or `tcpdump` on the media ports. The
   payloads don't decode as audio. Per-frame AES-128-GCM (`FrameCryptor`) is
   applied post-Opus/pre-SRTP, keyed from the channel's MLS exporter secret,
   which your server never receives. See
   [`pollis-core/src/commands/voice_e2ee.rs`](../pollis-core/src/commands/voice_e2ee.rs).
   - **Negative control:** to convince yourself the encryption is doing the
     work and not the transport, compare against what a normal (non-E2EE) SFU
     deployment would expose — there, the same capture yields clean audio.
   - **Membership binding:** remove one client from the group mid-call; MLS
     advances the epoch, the voice key rotates, and the removed client can no
     longer decrypt the next frame — without the server doing anything.

3. **The R2 objects are encrypted blobs.** Upload a file in the app, then
   download the object straight from your bucket. It's AES-256-GCM ciphertext,
   not the original file.

4. **The transparency log can't be forged behind your back.** Verify the
   append-only MLS commit log yourself, trusting only its signed public key —
   see [verify-transparency-log.md](verify-transparency-log.md).

If you can stand all of this up — with full control of every server — and still
can't read a message or hear a call, that's the claim demonstrated against the
strongest adversary there is: you, holding all the infrastructure.
