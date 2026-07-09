# Run Pollis yourself

Pollis is not "open source" by license, but it is **mechanically open**: the
repository is public, the desktop client *is* the backend (there is no
proprietary server in the middle), and nothing stops you from pointing the app
at infrastructure you own and running the whole thing end to end.

This guide is the honest version of "don't trust us — check." You provision your
own database, your own delivery service, your own media server, and your own file
storage, build the real client from this source tree, and watch it work. Because the security model
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
| **Delivery Service (DS)** | Sole writer to Turso — serializes MLS commits so the log stays append-only. Never sees plaintext. | Plain Docker image (`pollis-delivery/`) behind any reverse proxy |
| **LiveKit SFU** | Routes voice/screenshare RTP. Forwards ciphertext frames it cannot decode. | Self-host via [`livekit/`](../livekit/), or LiveKit Cloud |
| **Cloudflare R2** | Stores encrypted file/image blobs. | Any S3-compatible bucket |
| **Resend** | Delivers the email OTP sign-in code. | Optional — bypass with `DEV_OTP` |

The point of running all of it yourself: every byte these services hold is
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
# Add a keys: block to livekit/livekit.yml (it's omitted on purpose so no
# secret is committed — the comment at the bottom of the file shows the shape):
#   keys:
#     <API key>: <API secret>
docker compose -f livekit/docker-compose.yml up -d
```

> **Heads-up on `nginx.conf`:** the committed `livekit/nginx.conf` is Pollis's
> *production* ingress and still carries legacy `api`/`api-dev`/`deploy.pollis.com`
> server blocks (proxying to `delivery`/`watchtower`) from when the Delivery
> Service ran on this box. It no longer does — prod's DS moved to Cloudflare
> Containers (see step 4), so the VPS now fronts only LiveKit. For your own SFU you
> only need the LiveKit block: keep the single `server { … proxy_pass
> http://livekit:7880; }` (and the `:80` redirect), point `server_name` and the
> cert paths at your domain, and delete the `delivery`/`watchtower`/`api` blocks.
> (Pollis deploys LiveKit via the `workflow_dispatch` button in `livekit/DEPLOY.md`,
> but `docker compose up -d` on your box works exactly the same.)

Note the `wss://` URL and the API key/secret — they go in your `.env` next. For a
purely local sanity check you can also run `livekit-server --dev` and use its
well-known dev key/secret, but TURN/TLS won't be set up.

## 4. Run the Delivery Service (DS)

The DS (`pollis-delivery/`) is the **sole writer to Turso**: clients hold
read-only Turso tokens and POST every write (MLS commits, welcomes, membership)
to the DS, which serializes them so the commit log stays append-only. It's a
plain Rust HTTP server in a Docker image that reads plain env vars — no
Cloudflare (or any cloud) SDK is baked in. Build and run it from the repo root:

```bash
docker build -f pollis-delivery/Dockerfile -t pollis-delivery .
docker run -p 8788:8788 \
  -e TURSO_URL=libsql://pollis-selfhost-....turso.io \
  -e TURSO_TOKEN=<a read-write Turso token> \
  pollis-delivery
```

Only `TURSO_URL` + `TURSO_TOKEN` are required (`PORT` defaults to 8788). To let
the DS also broker LiveKit tokens and R2 presigns server-side — so those secrets
never ship in the client — pass the same `LIVEKIT_*` / `R2_*` vars you set in
step 6. Put it behind any reverse proxy that terminates TLS (nginx, Caddy,
Traefik, a tunnel — anything), then point the client at it with
`POLLIS_DELIVERY_URL` in step 6.

> **How the maintainers host prod — and why it doesn't constrain you.** Pollis's
> own prod/dev DS runs on [Cloudflare Containers](https://developers.cloudflare.com/containers/)
> (a Worker + Durable Object fronting this exact image) behind api.pollis.com /
> api-dev.pollis.com, with secrets synced from Doppler into Wrangler's Secrets
> Store. That's a hosting *choice*, not a requirement: the image is
> cloud-agnostic, so `docker run` behind your own proxy is the same server doing
> the same thing. Prod's orchestration (Wrangler deploys, scale-to-zero) differs
> from this self-host shape by design — the container inside is identical.

## 5. Create an R2 bucket (or any S3-compatible store)

From the Cloudflare dashboard: create an R2 bucket, generate an S3 access
key/secret, and note the S3 endpoint and a public URL for serving objects.

## 6. Configure credentials

Copy the sample and fill in everything you provisioned:

```bash
cp .env.example .env.development
```

```ini
TURSO_URL=libsql://pollis-selfhost-....turso.io
TURSO_TOKEN=<token from step 2>

POLLIS_DELIVERY_URL=https://<your-ds-domain>   # the reverse proxy in front of step 4

R2_ACCESS_KEY_ID=<from step 5>
R2_SECRET_KEY=<from step 5>
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

## 7. Run it

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

## 8. Build a real installer

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
