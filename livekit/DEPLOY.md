# LiveKit Server Deployment

## Stack
- LiveKit server (Docker)
- Nginx reverse proxy (Docker)
- Let's Encrypt SSL (host certbot)

## Directory Structure
```
livekit/
  docker-compose.yml
  livekit.yml
  nginx.conf
  README.md
```

---

## Fresh Server Setup

### 1. Install Docker
```bash
curl -fsSL https://get.docker.com | sh
```

### 2. Open firewall ports
```bash
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw allow 7881/tcp          # ICE TCP
sudo ufw allow 7882/udp          # ICE UDP
sudo ufw allow 5349/tcp          # TURNS (TURN over TLS, for VPN clients)
sudo ufw allow 30000:30100/udp   # TURN relay media ports
sudo ufw reload
```

Note: If your host (e.g. Hostinger) has a control panel firewall, open the same ports there too -- it overrides UFW.

### 3. Install certbot and generate SSL cert
```bash
sudo apt install certbot -y
sudo certbot certonly --standalone -d yourdomain.com
```

Certbot installs a systemd timer that auto-renews the cert before it expires. Certs expire every 90 days.

### 4. Clone or copy this directory onto the server
```bash
mkdir ~/livekit && cd ~/livekit
# copy docker-compose.yml, livekit.yml, nginx.conf here
```

### 5. Edit livekit.yml
Replace `your_api_key` and `your_api_secret` with your actual credentials:
```bash
vim livekit.yml
```

Generate a key/secret pair if you don't have one:
```bash
livekit-server generate-keys
```

### 6. Edit nginx.conf
Replace both instances of `yourdomain.com` with your actual domain:
```bash
vim nginx.conf
```

### 7. Start the stack
```bash
docker compose up -d
```

### 8. Verify
```bash
docker compose ps
curl https://yourdomain.com
```

Both containers should show as `Up` and the curl should return `OK`.

---

## App Connection

| Protocol | Endpoint |
|----------|----------|
| WebSocket (prod) | `wss://yourdomain.com` |
| HTTP API | `https://yourdomain.com` |

---

## SSL Renewal

Certbot renews automatically, but Nginx needs a restart to pick up the new cert:

```bash
sudo certbot renew
docker compose restart nginx
```

To automate this, add a cron job:
```bash
sudo crontab -e
```
```
0 3 * * * certbot renew --quiet && docker compose -f /root/livekit/docker-compose.yml restart nginx
```

---

## Useful Commands

```bash
docker compose up -d          # start stack
docker compose down           # stop stack
docker compose restart        # restart all services
docker compose ps             # status
docker compose logs -f        # live logs
docker compose logs livekit   # LiveKit logs only
docker compose logs nginx     # Nginx logs only
docker stats                  # live CPU/memory per container
```

---

## Performance Notes

- Each participant media track consumes one UDP port from the 50000-50200 range
- 200 ports handles ~20-30 concurrent users comfortably
- To expand the range, edit the UDP ports in docker-compose.yml (note: large ranges cause slow Docker startup)
- LiveKit exposes a Prometheus metrics endpoint at `/metrics` if you want to hook up monitoring later

### UDP receive buffer

LiveKit will warn at startup if the OS UDP receive buffer is below 5 MB (`UDP receive buffer is too small for a production set-up`). Under light load this doesn't matter, but at scale it causes packet drops and audio glitches. To fix:

```bash
# Apply immediately
sudo sysctl -w net.core.rmem_max=5000000
sudo sysctl -w net.core.rmem_default=5000000

# Persist across reboots
echo "net.core.rmem_max=5000000" | sudo tee -a /etc/sysctl.conf
echo "net.core.rmem_default=5000000" | sudo tee -a /etc/sysctl.conf
```