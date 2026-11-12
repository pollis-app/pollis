# LiveKit + nginx Deployment

This directory holds the **canonical, deployable** config for the LiveKit media
server and the shared nginx ingress on the Pollis VPS. `main` is the source of
truth — **do not hand-edit on the box**; deploys go through a button.

## Deploy (the button)

**Actions tab → "Deploy LiveKit + nginx" → Run workflow** (`prod` default).

`.github/workflows/livekit-deploy.yml` (#410) SSHes to the box, syncs this dir,
renders the LiveKit keys from secrets, and runs `docker compose up -d` + a
graceful nginx reload. No manual SSH. See "Workflow requirements" below.

## Restart (the other button)

**Actions tab → "Restart LiveKit + nginx" → Run workflow**, or archon's
**Restart voice server** (Overview → voice server).

`.github/workflows/livekit-restart.yml` SSHes in with the same key and host vars
and runs `docker compose up -d` in `/root/livekit`: it starts any
stopped container and touches nothing else (no config sync, no keys, no pull).
It fails unless every compose service is running, **both compose healthchecks
report `healthy` within two minutes**, and `https://rtc.pollis.com/` answers.
Tick `restart` to also `docker compose restart` both containers, for one that
is running but hung — that drops every live call.

Reach for it when joins fail with "Couldn't reach the voice server". After the
2026-10-17 kernel-update reboot, LiveKit came back but nginx exited 255 and
stayed down for six days despite `restart: unless-stopped`.

## Self-healing

What keeps the 2026-10-17 outage from recurring, and where each piece lives:

| Failure | Defence | Where |
|---------|---------|-------|
| nginx starts before `livekit` has a DNS entry → "host not found in upstream", exit 255 | `resolver 127.0.0.11 valid=10s` + variable `proxy_pass` targets in both the `http` and `stream` contexts: the name is resolved per connection, never at config load, so nginx boots regardless and follows a recreated container's new IP without a reload | `nginx.conf` |
| container stopped by a reboot / daemon restart | `restart: always` on both services (`unless-stopped` does not restart a container the daemon shutdown stopped) | `docker-compose.yml` |
| container up but not serving | healthchecks: `livekit` → `GET http://127.0.0.1:7880/` must answer; `nginx` → `GET http://127.0.0.1/healthz` (served by nginx itself on the port-80 vhost) must answer 200 | `docker-compose.yml`, `nginx.conf` |
| fresh `compose up` racing the boot | `depends_on: livekit: condition: service_healthy` on nginx (only at `compose up`; the daemon's boot-time restart ignores it, which is why the resolver is the real fix) | `docker-compose.yml` |
| a "successful" restart or deploy that recovered nothing | both workflows wait up to 120 s for every service's healthcheck to be `healthy` and fail loudly otherwise | `livekit-restart.yml`, `livekit-deploy.yml` |
| nobody notices for six days | `status.pollis.com` probes `https://rtc.pollis.com/` live on every page load ("Voice & video" row); no cron, by decision — a timer-driven probe was judged cost for ambiguous value | `archon` repo, `src/health.ts` |

`docker compose ps` shows the health column; `docker inspect -f '{{.State.Health.Status}}' livekit-nginx-1` gives it directly.

## Stack

| Service | Image (pinned) | Managed by |
|---------|----------------|------------|
| `livekit` | `livekit/livekit-server:v1.13.9` | this compose |
| `nginx` (ingress) | `nginx:1.30-alpine` | this compose |

These are the only containers on the VPS: the Delivery Service moved to
Cloudflare Containers (#515) and its `delivery` / `delivery-dev` / `watchtower`
containers were torn down (#555). Both share the `livekit_default` docker
network; nginx reaches `livekit` by service name through Docker's DNS.

```
livekit/
  docker-compose.yml   # livekit + nginx: ports, healthchecks, restart policy
  livekit.yml          # non-secret; deploy appends the keys: block from secrets
  nginx.conf           # LiveKit ingress: SNI demux on 443 (web + TURNS)
  DEPLOY.md
```

### Ingress routing (nginx.conf)

Everything arrives on **port 443** and is split by TLS SNI in nginx's `stream`
block (`ssl_preread`), because only one process can bind 443:

| SNI | Terminated by | Upstream | Cert |
|-----|---------------|----------|------|
| `rtc.pollis.com` | nginx `http` vhost | `livekit:7880` (HTTPS/WSS) | Let's Encrypt |
| `turn.pollis.com` | nginx `stream` server | `livekit:5349` (plaintext TURN) | same LE cert, via SAN |
| anything else / no SNI | nginx `http` vhost | `livekit:7880` | Let's Encrypt |

> **Why TURN is on 443 at all.** LiveKit hardcodes the ICE server URL it hands
> clients as `turns:<turn.domain>:443?transport=tcp` — it does **not**
> interpolate `tls_port`. While `livekit.yml` said `tls_port: 5349`, clients were
> being told to reach TURNS on 443, where nginx answered with HTTP; TURNS never
> worked, and it fails silently (ICE just never finds a working candidate). 443
> is also the port restrictive corporate/hotel/carrier networks actually let
> through — the networks whose users need a relay in the first place.

> **Why nginx terminates the TURN TLS** (`turn.external_tls: true` in
> `livekit.yml`) instead of passing it through to LiveKit's own TLS listener:
> the certbot cron reloads **nginx**, whereas LiveKit reads its certificate once
> at process start. A LiveKit-terminated listener would serve an expired cert
> ~60 days after every renewal. LiveKit no longer gets `/etc/letsencrypt`
> mounted at all.

> **No ordering dependency any more:** both `proxy_pass` targets are variables
> resolved per connection through Docker's DNS (`resolver 127.0.0.11
> valid=10s`), so nginx starts and reloads whether or not `livekit` is
> resolvable at that moment — a literal upstream name was resolved once at
> config load and was fatal when unknown, which is what took the ingress down
> on 2026-10-17 (see "Self-healing"). An unresolvable name now costs a 502 on
> that request, not the process.

> **Connection accounting:** a 443 connection now costs **four** `worker_connections`
> slots (stream accept, stream→loopback, loopback accept, upstream) rather than
> two, which is why `worker_connections` moved 8192 → 16384. The loopback hop
> carries **no** PROXY protocol header: it only ever fed nginx's access logs,
> and those are gone (next section).

## No client IPs in logs

No client IP may be readable on this box, operators included. Everything that
could write one is off:

| Source | Setting |
|--------|---------|
| nginx access logs (`http` + `stream`) | `access_log off;` in `nginx.conf` |
| nginx error log | `error_log /dev/null crit;` (top level) |
| PROXY protocol / `real_ip` on the loopback hop | removed — nothing reads the client address |
| LiveKit process | `logging: { level: warn, pion_level: error }` in `livekit.yml` |
| Container stdout/stderr (both) | `logging: driver: none` in `docker-compose.yml` |

LiveKit still learns client addresses for ICE from the media ports (7881,
7882-7883, 3478, the TURN relay range), which docker publishes by iptables DNAT — that is how
WebRTC works and nothing records it. `docker compose logs` now prints nothing;
diagnose with `nginx -t` / `nginx -T`, `docker compose ps` and the deploy
workflow's checks.

**Rolling this out (once, on the first deploy after the change):**

1. The deploy's `docker compose up -d` recreates both containers, because their
   `logging` changed; recreating a container deletes its old `json-file` log.
   If in doubt, run `docker compose up -d --force-recreate` on the box.
2. The deploy now runs `ufw logging off` every time and fails if either
   container logs with anything but the `none` driver. For the one-time cleanup
   of logs written before this change, dispatch the deploy with
   `scrub_host_logs: true` — it does what is listed below. By hand, the same:
   ```bash
   ufw logging off                                   # UFW logs every blocked packet's source IP
   truncate -s 0 /var/lib/docker/containers/*/*-json.log   # any container log left behind
   ls -la /var/log/ufw.log* /var/log/kern.log* /var/log/syslog* /var/log/dmesg* /var/log/nginx 2>/dev/null
   dmesg -C                                          # the live kernel ring buffer holds UFW lines too
   journalctl --disk-usage                           # journald keeps kernel/UFW lines too
   journalctl --vacuum-time=1s                       # drop what is already there
   ```
   and set a short retention for anything that must stay (e.g.
   `SystemMaxRetentionSec=1d` in `/etc/systemd/journald.conf`), removing rotated
   `ufw.log*` / `kern.log*` / `dmesg.*` files that predate `ufw logging off`
   (and stripping `[UFW ` lines from the live `kern.log`, `syslog` and `dmesg`).

## Workflow requirements (one-time)

- **Secrets:** `VPS_SSH_KEY` (deploy private key; add the pubkey to the box's
  `authorized_keys`), and **two** LiveKit pairs — `LIVEKIT_API_KEY` /
  `LIVEKIT_API_SECRET` (prod) and `LIVEKIT_DEV_API_KEY` /
  `LIVEKIT_DEV_API_SECRET` (dev). Both must match the corresponding Doppler
  config, since the DS is what mints tokens; see "LiveKit API keys" below.
- **Variables:** `VPS_HOST` (`31.97.140.76`), `VPS_USER` (`root`).
- **GitHub Environment** `livekit-prod` (+ `livekit-dev` if used) for the
  optional manual-approval gate.

## Fresh-box provisioning (one-time, manual)

The deploy workflow assumes the box already has Docker, the firewall, certs, and
the host tunables in place. On a brand-new box:

### 1. Docker + firewall
```bash
curl -fsSL https://get.docker.com | sh
ufw allow 80/tcp && ufw allow 443/tcp
ufw allow 7881/tcp          # ICE TCP
ufw allow 7882:7883/udp     # ICE UDP mux range (rtc.udp_port — one port per vCPU)
ufw allow 3478/udp          # TURN over UDP (doubles as the STUN server clients are handed)
ufw allow 30000:30999/udp   # TURN relay media ports
ufw reload
```
Upgrading an existing box: `ufw allow 3478/udp && ufw allow 7882:7883/udp`
(the old single `7882/udp` rule can stay). The deploy workflow refuses to run
until both are there.
Note: if the host (e.g. Hostinger) has a control-panel firewall, open the same
ports there too — it overrides UFW.

**5349 is deliberately NOT open and no longer published by docker.** TURNS
arrives on 443, nginx terminates it and forwards plaintext TURN over the compose
network to `livekit:5349`; nothing outside nginx should reach that listener. If
you are upgrading an existing box, drop the old rules:
```bash
ufw delete allow 5349/tcp
ufw delete allow 30000:30100/udp
```

**Docker's iptables rules override UFW** for *published* container ports, so
UFW alone cannot close a published port — that is why 5349 was removed from the
`ports:` list in `docker-compose.yml` rather than just firewalled off.

### 1b. Disable Docker's userland proxy (**required**)
```bash
# /etc/docker/daemon.json
{ "userland-proxy": false }
```
```bash
systemctl restart docker
```
Docker forks **two `docker-proxy` processes per published port**. Measured
locally: a 1000-port UDP range → 2000 processes and ~22 s of container start;
a 250-port range → 500 processes and ~5.6 s. At the relay range this stack now
publishes, that would exhaust the 7 GB box. With the userland proxy disabled the
whole contiguous range collapses into iptables DNAT and costs essentially
nothing. **The deploy workflow hard-fails if this setting is absent**, before it
touches the stack.

### 2. Certs
- **Let's Encrypt** — one certificate covering **both** `rtc.pollis.com` and
  `turn.pollis.com` (nginx serves the same lineage on both SNIs, so
  `turn.pollis.com` must be a SAN or clients' TLS verification fails and TURN
  silently never connects). `--standalone` needs port 80, which nginx holds:
  ```bash
  apt install certbot -y
  cd /root/livekit && docker compose stop nginx
  certbot certonly --standalone --expand -d rtc.pollis.com -d turn.pollis.com
  docker compose start nginx
  ```
  `--expand` keeps the lineage named `rtc.pollis.com`, so the paths in
  `nginx.conf` do not move. Verify:
  ```bash
  openssl x509 -in /etc/letsencrypt/live/rtc.pollis.com/fullchain.pem -noout -text \
    | grep -A1 'Subject Alternative Name'
  ```
  Auto-renews via a systemd timer; nginx must reload to pick up renewals:
  ```bash
  # crontab -e
  0 3 * * * certbot renew --quiet && docker compose -f /root/livekit/docker-compose.yml exec -T nginx nginx -s reload
  ```
  Renewal is `--standalone` too, so the timer needs nginx off port 80 for the
  moment it runs; if renewals start failing, switch the lineage to `--webroot`.

  **How the served chain is produced.** nginx serves
  `/etc/letsencrypt/live/rtc.pollis.com/fullchain.pem` verbatim on both SNIs;
  certbot writes that file at every renewal from the chain the ACME server
  returns, so the chain is a property of the *lineage's renewal config*
  (`/etc/letsencrypt/renewal/rtc.pollis.com.conf`), not of anything in this
  repo. Let's Encrypt's default ECDSA chain today is **four** certificates —
  leaf ← `YE2` ← `ISRG Root YE` (cross-signed by `ISRG Root X2`) ← `ISRG Root
  X2` (cross-signed by `ISRG Root X1`), 4,884 bytes on the wire; the last one
  (2,160 bytes) exists only for clients that trust X1 but not X2. The
  shorter alternative is the X2-anchored chain (drops that last cert), chosen
  by certbot's `--preferred-chain`, which persists across renewals once it is
  in the renewal config:
  ```bash
  # /etc/letsencrypt/renewal/rtc.pollis.com.conf, under [renewalparams]:
  preferred_chain = ISRG Root X2
  # then re-issue once (standalone needs port 80, as above):
  cd /root/livekit && docker compose stop nginx
  certbot renew --cert-name rtc.pollis.com --force-renewal
  docker compose start nginx
  # verify: 3 certs, topmost issued by ISRG Root X2
  echo | openssl s_client -connect rtc.pollis.com:443 -servername rtc.pollis.com -showcerts 2>/dev/null | grep -c 'BEGIN CERT'
  ```
  (`certbot certonly … --preferred-chain "ISRG Root X2"` does the same on a
  fresh box; a preference that matches no offered chain falls back to the
  default with a warning, so it cannot leave the lineage without a cert.)

  **Decide before applying it.** Per Let's Encrypt's compatibility list, ISRG
  Root X2 is trusted *directly* only by Android ≥ 14, iOS ≥ 16 / macOS ≥ 13,
  Firefox ≥ 97 and Windows with root auto-update. Desktop Pollis verifies
  through rustls' bundled Mozilla roots (`livekit` crate feature
  `rustls-tls-webpki-roots`), which include X2 — fine. The mobile app's
  realtime rooms, however, connect through the native LiveKit SDK on the **OS
  trust store**: iOS ≥ 16.4 (the app's floor) is fine, but an **Android 7–13
  phone would fail TLS to `rtc.pollis.com`** and lose every realtime room.
  The saving is ~2 KB of the first handshake per connection — about one TCP
  segment within the initial congestion window, nowhere near a round trip.
  Recommendation: leave the default (X1-anchored) chain until the mobile
  Android floor is 14, and note the choice here when it changes. Rollback is
  removing the `preferred_chain` line and renewing again.

  **Session resumption.** Both TLS terminators in `nginx.conf` keep sessions
  for a day (`ssl_session_timeout 1d`, shared caches `SSL_WEB` / `SSL_TURN`,
  tickets on) so the app's start-up warmup still pays off at join time; nginx's
  default was five minutes. Check it is live:
  ```bash
  echo | openssl s_client -connect rtc.pollis.com:443 -servername rtc.pollis.com -sess_out /tmp/s 2>/dev/null | grep -i 'session-id\|lifetime'
  sleep 400   # past the old 5-minute window
  echo | openssl s_client -connect rtc.pollis.com:443 -servername rtc.pollis.com -sess_in /tmp/s 2>/dev/null | grep -i 'reused'
  ```
  `TLS session ticket lifetime hint: 86400` and `Reused, TLSv1.3` are the pass
  conditions. A reload regenerates the ticket key, so outstanding tickets stop
  resuming after a cert renewal — expected, one full handshake per client.
- **DNS:** `turn.pollis.com` needs an **A record straight to the VPS
  (`31.97.140.76`), DNS-only — no Cloudflare proxy.** A proxied record resolves
  to Cloudflare, which does not speak TURN. The deploy workflow refuses to run
  unless `turn.pollis.com` resolves to `VPS_HOST`.
- **Cloudflare Origin cert** at `/etc/ssl/cloudflare/verify.pollis.com.{pem,key}`
  — a leftover from when `api`/`api-dev`/`deploy.pollis.com` were proxied here.
  The Delivery Service moved to Cloudflare Containers (#515); the mount remains
  but nothing in `nginx.conf` reads it.

### 3. Host tunables (UDP receive buffer)
LiveKit warns and drops packets under load if the OS UDP receive buffer is below
5 MB. Apply once (already applied on the current box):
```bash
sysctl -w net.core.rmem_max=5000000
sysctl -w net.core.rmem_default=5000000
echo "net.core.rmem_max=5000000"     >> /etc/sysctl.conf
echo "net.core.rmem_default=5000000" >> /etc/sysctl.conf
```

### 4. Deploy
Hit the **Deploy LiveKit + nginx** button, then open `status.pollis.com` and check the "Voice & video" row is green.

## LiveKit API keys

One server, **two** key pairs — both DS deployments (`api.pollis.com` and
`api-dev.pollis.com`) connect to `wss://rtc.pollis.com`, and LiveKit rejects a
token signed by any key absent from its `keys:` block.

| Pair | Doppler config | GitHub secret | Used by |
|------|----------------|---------------|---------|
| prod | `pollis/prd_prod` | `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET` | prod DS |
| dev  | `pollis/dev_personal` | `LIVEKIT_DEV_API_KEY` / `LIVEKIT_DEV_API_SECRET` | dev DS, local `.env.development` |

Note the asymmetry: **both** Doppler configs name the secrets `LIVEKIT_API_KEY`
/ `LIVEKIT_API_SECRET` (that is what `broker.rs` reads, and each DS only ever
sees its own config); the `LIVEKIT_DEV_*` names exist only on the GitHub side,
where one workflow needs both pairs at once to render them into a single file.

The pairs must stay distinct — the DS derives room names and participant
identities by HMAC over `LIVEKIT_API_SECRET`, so a shared secret would give dev
and prod the same room pseudonym for the same conversation.

### Rotating

Rotation is self-healing rather than a break: it invalidates outstanding tokens,
and clients re-request one in a round trip they were already going to make. Only
calls in progress at the moment of the switch drop. Rooms are re-keyed (see
`room_id.rs`), which is expected.

```bash
# 1. Generate (canonical format, from the pinned image)
docker run --rm --entrypoint /livekit-server \
  livekit/livekit-server:v1.13.9 generate-keys
```

2. Write the new values to Doppler (`pollis/prd_prod`, `pollis/dev_personal`),
   the GitHub repo secrets, and your local `.env.production` / `.env.development`.
3. **Deploy LiveKit first** (this workflow) so the server accepts the new keys.
4. Then re-run **Deploy Delivery Service (prod)** and **(dev)** — the DS reads
   its keys from the Cloudflare Secrets Store, which `sync-ds-secrets.sh`
   repopulates from Doppler on each deploy. Until that runs, the DS keeps
   minting tokens with the old key and the server refuses them.

## App connection

| Protocol | Endpoint |
|----------|----------|
| WebSocket (prod) | `wss://rtc.pollis.com` |
| HTTP API | `https://rtc.pollis.com` |
| TURN/UDP + STUN | `turn:31.97.140.76:3478?transport=udp` |
| TURNS (relay fallback) | `turns:turn.pollis.com:443?transport=tcp` |

Clients do not configure TURN or STUN — LiveKit hands both URLs above to every
participant in the ICE server list, with per-participant credentials. The
`:443` is hardcoded in LiveKit, not derived from `turn.tls_port`; the UDP URL
is built from `rtc.node_ip` + `turn.udp_port`. **Without `turn.udp_port`
LiveKit hands out Google's public STUN servers instead** (`DefaultStunServers`
in `pkg/service/roommanager.go`), which disclosed every client's IP to a third
party on every join (`docs/metadata-retention-policy.md` §3) — keep the UDP
listener configured, published and open.

## DS server-API tunnel (#1751)

Cloudflare Container egress to this box is dropped upstream of it, so the
Delivery Service reaches LiveKit's server API (Twirp `RoomService` and `GET /`)
through a Cloudflare Tunnel instead of `rtc.pollis.com`. Clients are unaffected
and keep using `rtc.pollis.com`.

- **Tunnel:** `pollis-livekit-api`, remotely managed (its ingress lives in the
  Cloudflare dashboard, Zero Trust → Networks → Tunnels):
  `livekit-api.pollis.com` → `http://livekit:7880`, everything else 404.
- **DNS:** `livekit-api.pollis.com`, a proxied CNAME to the tunnel. One hostname
  serves dev and prod, since both use this one LiveKit with separate key pairs.
  Set `LIVEKIT_API_URL=https://livekit-api.pollis.com` in Doppler
  (`dev_personal`, `prd_prod`), then deploy the DS with `force_restart: true`.
- **Edge rule:** a WAF custom rule on the zone blocks every request to that
  hostname except `GET`/`HEAD /` and `POST /twirp/livekit.RoomService/*`.
  LiveKit's own API-key JWTs still authenticate every call.
- **On the box:** a standalone container `cloudflared-livekit-api`
  (`--restart always`, `--log-driver none`, read-only, no capabilities) on the
  compose network, so it reaches `livekit:7880` without publishing that port.
  It is outside `docker-compose.yml`, so a LiveKit deploy or restart leaves it
  alone. Its run token is in `/root/cloudflared/livekit-api.env` (mode 600) and
  nowhere else. On a rebuild, copy a fresh token from the tunnel's page in the
  dashboard and re-run:
  ```bash
  NET=$(docker inspect -f '{{range $k, $v := .NetworkSettings.Networks}}{{$k}}{{end}}' "$(docker compose ps -q livekit)")
  docker run -d --name cloudflared-livekit-api --restart always --network "$NET" \
    --log-driver none --env-file /root/cloudflared/livekit-api.env \
    --read-only --cap-drop ALL --security-opt no-new-privileges --memory 256m \
    cloudflare/cloudflared:2026.10.3@sha256:faa8e0a1475f137c910be6025f43c8e4092fdd35461b31e19fd1390657a1ff39 \
    tunnel --no-autoupdate --loglevel error run
  ```
- **Check:** `curl -s https://livekit-api.pollis.com/` answers `OK`; 530 means
  the connector is down (`docker ps --filter name=cloudflared-livekit-api`).
  From the DS: `/v1/ops/livekit?probe=1` reports
  `api_base_source: LIVEKIT_API_URL`.

## Useful commands (on the box)

```bash
cd /root/livekit
docker compose ps                 # status + health column (livekit + nginx)
# (no `docker compose logs` — container logging is off; see "No client IPs in logs")
docker compose exec nginx nginx -t            # validate ingress config
docker compose exec nginx nginx -s reload     # graceful reload after a cert renew
docker stats                      # live CPU/memory per container
docker inspect -f '{{.State.Health.Status}}' livekit-livekit-1 livekit-nginx-1
curl -s http://127.0.0.1:6789/metrics | grep -E '^livekit_(room|participant)_total'
```

## Useful commands (TURN)

```bash
# Does 443 speak TURN under the turn SNI? (should NOT answer HTTP)
openssl s_client -connect turn.pollis.com:443 -servername turn.pollis.com \
  -verify_hostname turn.pollis.com -verify_return_error -brief

# Does 443 still speak HTTPS under the web SNI?
curl -sS https://rtc.pollis.com

# How many relay ports are actually in use right now
docker compose exec -T livekit sh -c 'ss -lun' | awk '$5 ~ /:3[0-9]{4}$/' | wc -l
```

## Capacity

The box is a Hostinger KVM 2 (2 vCPU, 8 GB, **8 TB/month transfer**) in
Boston. What bounds it, in order:

1. **Peer connections, not CPU.** Every signed-in client holds a LiveKit
   realtime room per conversation plus its inbox — about **12 peer connections
   (and 12 WSS sessions) per user** before anyone speaks
   (`pollis-core/src/commands/livekit/realtime.rs`,
   `frontend/src/hooks/useLiveKitRealtime.ts`). The 2026-08-11 load test ran
   150 subscribers at 13 % CPU with zero failures; failures above that were
   nginx's connection ceiling (#817, since raised), not the SFU. Budget
   **~150 concurrent users** on this box, set by peer-connection count; the
   lever that moves it is fewer realtime rooms per user (one data room per
   user, or a DS WebSocket), which is a client/DS change, not a server one.
2. **Transfer cap.** 8 TB/month is **~25 Mbit/s averaged over the month** —
   about **13 continuous 720p streams** at the client's ~1.9 Mbit/s 720p
   bitrate, or ~800 continuous Opus voice streams. Audio-only usage will never
   hit it; sustained video/screenshare will, long before CPU does. Watch
   `rate(livekit_node_packet_total[5m])` ("Metrics") and the host's monthly
   transfer in the Hostinger panel. `limit.bytes_per_sec` (60 MB/s) only
   stops a burst from saturating the NIC; it is not a monthly budget.
3. **Relay ports: 30000–30999 (1000 ports).** One UDP port per TURN
   *allocation*, one allocation per relayed peer connection, and LiveKit 1.13
   SDKs negotiate a **single** peer connection per participant — so 1000 ports
   ≈ 1000 relayed peer connections, with slack for allocations still
   draining. A user whose network forces *every* connection through the
   relay costs ~12 of them (item 1), so the range covers ~80 fully relayed
   users; relay is the minority path (UDP-blocking networks), so this is not
   the first ceiling. The old 101-port range capped relayed peer connections
   at ~100 regardless of how idle the CPU was.
4. **Latency, not capacity.** One node in Boston means ~120 ms connect for
   US-East, ~0.6 s for Europe and ~1.6 s for APAC (seven round trips: TCP,
   TLS, WSS+join, SDP, ICE, DTLS ×2), and 180–250 ms one-way media for APAC.
   Only more nodes fix that — "Tier 2" below.

Changing the relay range means editing **three** places in lockstep —
`relay_range_*` in `livekit.yml`, the `ports:` publish in
`docker-compose.yml`, and the host firewall. Ports LiveKit allocates but the
firewall drops produce silent relay failures. The same lockstep applies to
the ICE/UDP mux range (`rtc.udp_port`) and the TURN/UDP port (`turn.udp_port`).

## Tier 2 — regional nodes (runbook)

When APAC/EU latency matters more than the ~$10–15/month per extra node. Two
or three LiveKit nodes, one per region, sharing one Redis; a room lives on
the node its first participant joined and other nodes relay signalling to it
(one cross-region media leg per remote participant — LiveKit OSS routes by
*room*, not per-participant mesh). Clients need **no change**: the DS already
returns the SFU `url` with every token (`LivekitTokenResponse { token, url }`,
honoured by desktop `resolve_livekit_url` and mobile).

**Prerequisite:** reduce the per-user realtime-room fan-out first (Capacity
item 1); every extra node multiplies nothing if each user still opens twelve
peer connections to whichever node they land on.

1. **Redis.** One instance reachable from every node over a private link or
   TLS (`redis.address`, `redis.password`, `redis.tls` in `livekit.yml`;
   every key is in the v1.13.9 config sample). A managed Redis in the first
   region, or `redis:7-alpine` in the first node's compose with its port
   firewalled to the other nodes' IPs. Redis holds room → node routing and
   the node list; no media, no identities beyond LiveKit's room pseudonyms.
2. **Per-node `livekit.yml`.** Identical except:
   ```yaml
   region: us-east            # or eu-west, ap-southeast — free-form, must match node_selector.regions
   rtc:
     node_ip: <that node's public IP>
   turn:
     domain: turn-<region>.pollis.com
   node_selector:
     kind: regionaware
     sort_by: sysload
     sysload_limit: 0.7
     regions:
       - { name: us-east,      lat: 42.36,  lon: -71.06 }
       - { name: eu-west,      lat: 50.11,  lon: 8.68 }
       - { name: ap-southeast, lat: 1.35,   lon: 103.82 }
   ```
   Same `keys:` block on every node (the deploy renders it from the same
   secrets). `regionaware` picks the node nearest the *first joiner*; the
   node a client signals to is chosen by the DS (step 4), so the two agree.
3. **Per-node ingress.** Each node runs this compose unchanged with its own
   nginx, its own DNS-only A records (`rtc-<region>.pollis.com`,
   `turn-<region>.pollis.com` → that node) and its own Let's Encrypt lineage
   with both names as SANs (§2 above, per node). `turn.domain` is per node
   because LiveKit hands clients `turns:<turn.domain>:443`, and the relay
   must be the node they are on. The deploy workflow grows a `node` input
   (host, user, `REMOTE_DIR`) or a matrix over `vars.VPS_HOST_<REGION>`; the
   per-node preflights (DNS, SAN, firewall) stay as they are.
4. **DS: pick the URL by request country.** The Cloudflare Worker in front of
   the DS sees `request.cf.country` for free. `pollis-delivery/worker/index.ts`
   maps it to a region name (`us-east` default; a short EU country list →
   `eu-west`; AU/NZ/SG/JP/KR/IN… → `ap-southeast`) and forwards **only the
   region** as `X-Pollis-Region`, the way it already forwards the hashed
   client bucket — the country itself is never forwarded, stored or logged
   (`docs/metadata-retention-policy.md` §3; extend
   `tests/no_client_ip_exposure.rs` to assert the header carries a region
   name from a fixed set). The DS `broker.rs` reads
   `LIVEKIT_URL_<REGION>` (falling back to `LIVEKIT_URL`) when minting
   `/v1/livekit/token` and returns that `url`. Additive: a DS without the
   header or the vars behaves exactly as today, and every shipped client
   already follows `url`.
5. **Status page.** One "Voice & video" row per node on `status.pollis.com`
   (archon `probeVoice`, live on page load).
6. **Verify.** From each region: `curl https://rtc-<region>.pollis.com` → `OK`;
   a token minted via that region's DS answers with the matching `url`; two
   clients in different regions join one room and `livekit_participant_total`
   rises on exactly one node while the other shows the relayed signalling.

Tier 3 (not planned): LiveKit Cloud (multi-home mesh, E2EE-compatible, but a
third party then sees client IPs and pseudonymous room metadata) or an
autoscaled OSS fleet on the relay-hydra Terraform pattern.
Do not widen the relay range much further without re-measuring: every
published port is iptables state, and with the userland proxy accidentally
re-enabled it is also two processes per port (see §1b). Metrics are on a
separate loopback-only port, not on `/metrics` of the public vhost — see
"Metrics".

## Metrics

LiveKit serves Prometheus metrics only when `prometheus.port` is set
(`livekit.yml`: `6789`), and only on that port — `https://rtc.pollis.com/metrics`
is a 404 and must stay one. `docker-compose.yml` publishes the port as
`127.0.0.1:6789:6789`, so on the box:

```bash
curl -s http://127.0.0.1:6789/metrics | head
```

and nothing off the box can reach it (no firewall rule, no nginx route). The
series are node-level — rooms, participants, tracks, bytes, packet loss, CPU —
with no participant identity and no client address.

**Minimal scrape recipe — Grafana Alloy → Grafana Cloud (free tier).** Alloy
runs on the box as a systemd service and pushes to a hosted Prometheus; nothing
is stored on the VPS beyond Alloy's WAL.

1. Create a free Grafana Cloud stack; under *Connections → Add new connection →
   Hosted Prometheus metrics* take the remote-write URL, the instance id
   (username) and an API token with the `MetricsPublisher` role. Keep the token
   in Doppler (`prd_prod`, `GRAFANA_CLOUD_METRICS_TOKEN`); it is a push-only
   credential.
2. Install Alloy from Grafana's apt repo
   (`https://grafana.com/docs/alloy/latest/set-up/install/linux/`) and write
   `/etc/alloy/config.alloy`:
   ```alloy
   prometheus.scrape "livekit" {
     targets    = [{ __address__ = "127.0.0.1:6789", job = "livekit" }]
     forward_to = [prometheus.remote_write.grafana_cloud.receiver]
     scrape_interval = "30s"
   }

   prometheus.remote_write "grafana_cloud" {
     endpoint {
       url = "https://<stack>.grafana.net/api/prom/push"
       basic_auth {
         username      = "<instance id>"
         password_file = "/etc/alloy/grafana-cloud-token"
       }
     }
   }
   ```
   with the token in `/etc/alloy/grafana-cloud-token` (`chmod 600`, owner
   `alloy`), then `systemctl enable --now alloy`.
3. Verify in Grafana Cloud's Explore: `livekit_room_total` and
   `livekit_participant_total` (both labelled by `node_id`), `process_cpu_seconds_total`;
   `livekit_node_packet_total`, `livekit_forward_latency` and the
   `livekit_quality_score` histogram appear once media flows. Useful first
   alerts: `up{job="livekit"} == 0` for 5 min (scraper cannot reach LiveKit;
   `status.pollis.com` shows the public path), and
   `rate(livekit_node_packet_total[5m])` against the transfer budget in
   "Capacity".

Alloy sees only the loopback endpoint; it never touches client traffic.
