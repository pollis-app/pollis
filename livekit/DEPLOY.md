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
  nginx.conf           # full ingress: rtc, downpage (LiveKit) + api, api-dev, deploy (delivery)
  DEPLOY.md
```

### Ingress routing (nginx.conf)

| Hostname | Upstream | Cert |
|----------|----------|------|
| `rtc.pollis.com` | `livekit:7880` | Let's Encrypt |
| `downpage.xyz` | `livekit:7880` (legacy) | Let's Encrypt |
| `api.pollis.com` | `delivery:8788` | Cloudflare Origin (`/etc/ssl/cloudflare/verify.pollis.com.*`) |
| `api-dev.pollis.com` | `delivery-dev:8788` | Cloudflare Origin |
| `deploy.pollis.com` | `watchtower:8080` | Cloudflare Origin |

> **Ordering dependency:** nginx resolves `proxy_pass` upstreams at config load,
> so the `delivery`/`delivery-dev`/`watchtower` containers must be **up before
> nginx starts or reloads** — otherwise nginx fails with "host not found in
> upstream". On a fresh box, deploy the Delivery Service (#407) first. The deploy
> workflow runs `nginx -t` before reloading to catch this.

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
ufw allow 5349/tcp          # TURNS (TURN over TLS, for VPNs)
ufw allow 30000:30100/udp   # TURN relay media ports
ufw reload
```
Note: if the host (e.g. Hostinger) has a control-panel firewall, open the same
ports there too — it overrides UFW.

### 2. Certs
- **Let's Encrypt** (`rtc.pollis.com`, `downpage.xyz`) via host certbot:
  ```bash
  apt install certbot -y
  certbot certonly --standalone -d rtc.pollis.com -d downpage.xyz
  ```
  Auto-renews via a systemd timer; nginx must reload to pick up renewals:
  ```bash
  # crontab -e
  0 3 * * * certbot renew --quiet && docker compose -f /root/livekit/docker-compose.yml exec -T nginx nginx -s reload
  ```
- **Cloudflare Origin cert** for `api`/`api-dev`/`deploy.pollis.com` at
  `/etc/ssl/cloudflare/verify.pollis.com.{pem,key}` (Cloudflare runs Full
  (strict) in front of these).

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

## Useful commands (on the box)

```bash
cd /root/livekit
docker compose ps                 # status (livekit + nginx)
docker compose logs -f livekit    # LiveKit logs
docker compose exec nginx nginx -t            # validate ingress config
docker compose exec nginx nginx -s reload     # graceful reload after a cert renew
docker stats                      # live CPU/memory per container
```

## Performance notes

- Each participant media track consumes one UDP port from the 30000–30100 range
  (TURN relay); ~100 ports handles a healthy number of concurrent users.
- LiveKit exposes Prometheus metrics at `/metrics` for future monitoring.
- Expanding the UDP range means editing the port mapping in `docker-compose.yml`
  *and* the firewall; large ranges slow Docker startup.
