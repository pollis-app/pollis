# LiveKit + nginx Deployment

This directory holds the **canonical, deployable** config for the LiveKit media
server and the shared nginx ingress on the Pollis VPS. `main` is the source of
truth — **do not hand-edit on the box**; deploys go through a button.

## Deploy (the button)

**Actions tab → "Deploy LiveKit + nginx" → Run workflow** (`prod` default).

`.github/workflows/livekit-deploy.yml` (#410) SSHes to the box, syncs this dir,
renders the LiveKit keys from secrets, and runs `docker compose up -d` + a
graceful nginx reload. No manual SSH. See "Workflow requirements" below.

## Stack

| Service | Image (pinned) | Managed by |
|---------|----------------|------------|
| `livekit` | `livekit/livekit-server:v1.10.0` | this compose |
| `nginx` (shared ingress) | `nginx:1.29-alpine` | this compose |
| `delivery` / `delivery-dev` | `ghcr.io/actuallydan/pollis-delivery:{prod,dev}` | **#407** (Watchtower) — standalone |
| `watchtower` | `containrrr/watchtower` | **#407** — standalone |

All five containers share the `livekit_default` docker network. nginx reaches
the delivery/watchtower containers by network alias.

```
livekit/
  docker-compose.yml   # livekit + nginx only (delivery/watchtower owned by #407)
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

> **Ordering dependency:** nginx resolves `proxy_pass` upstreams at config load,
> so upstream containers must be **up before nginx starts or reloads** —
> otherwise nginx fails with "host not found in upstream". The deploy workflow
> runs `nginx -t` before reloading to catch this.

> **Connection accounting:** a 443 connection now costs **four** `worker_connections`
> slots (stream accept, stream→loopback, loopback accept, upstream) rather than
> two, which is why `worker_connections` moved 8192 → 16384. PROXY protocol
> carries the real client IP across the loopback hop, so access logs keep showing
> real addresses instead of `127.0.0.1`.

## Workflow requirements (one-time)

- **Secrets:** `VPS_SSH_KEY` (deploy private key; add the pubkey to the box's
  `authorized_keys`), `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET` (must match what
  shipped clients use).
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
ufw allow 7882/udp          # ICE UDP
ufw allow 30000:30999/udp   # TURN relay media ports
ufw reload
```
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

### 4. Delivery Service
Stand up `delivery` / `delivery-dev` / `watchtower` (#407) so nginx's upstreams
resolve, then hit the **Deploy LiveKit + nginx** button.

## App connection

| Protocol | Endpoint |
|----------|----------|
| WebSocket (prod) | `wss://rtc.pollis.com` |
| HTTP API | `https://rtc.pollis.com` |
| TURNS (relay fallback) | `turns:turn.pollis.com:443?transport=tcp` |

Clients do not configure TURN — LiveKit hands the URL above to every participant
in the ICE server list. The `:443` is hardcoded in LiveKit, not derived from
`turn.tls_port`.

## Useful commands (on the box)

```bash
cd /root/livekit
docker compose ps                 # status (livekit + nginx)
docker compose logs -f livekit    # LiveKit logs
docker compose exec nginx nginx -t            # validate ingress config
docker compose exec nginx nginx -s reload     # graceful reload after a cert renew
docker stats                      # live CPU/memory per container
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

## Performance notes

- **Relay ports: 30000–30999 (1000 ports).** One UDP port per TURN *allocation*,
  and a LiveKit client opens **two** peer connections (publisher + subscriber),
  so budget ~2 ports per concurrent relayed participant. 1000 ports ≈ 250
  concurrent participants with slack for allocations still draining —
  comfortably past what 2 vCPU can serve, and past the 150-subscriber level that
  load-tested clean. The old 101-port range capped this at ~50 participants
  regardless of how idle the CPU was.
- Changing the range means editing **three** places in lockstep —
  `relay_range_*` in `livekit.yml`, the `ports:` publish in
  `docker-compose.yml`, and the host firewall. Ports LiveKit allocates but the
  firewall drops produce silent relay failures.
- Do not widen much further without re-measuring: every published port is
  iptables state, and with the userland proxy accidentally re-enabled it is also
  two processes per port (see §1b).
- LiveKit exposes Prometheus metrics at `/metrics` for future monitoring.
