# pollis-bot

The Pollis bot runtime. It runs on a machine you control, hosts one or more bots, and posts into their group when told to. Design and internals: `.codesight/wiki/bots.md` ("Runtime").

A bot can read every message in its group, because it is a member. Run it only on a machine you trust with that.

## Install

```bash
cargo build --release -p pollis-bot
install -m 755 target/release/pollis-bot ~/.local/bin/
```

The runtime talks to the Delivery Service set in the environment (`POLLIS_DELIVERY_URL`, as for the `pollis` CLI) and needs no PIN: each bot's keys sit in the OS keychain, or where there is none, an encrypted file bound to the machine.

## Pair a new bot

```bash
pollis-bot pair --name "monitoring box"
```

It prints an eight-character code and waits. A group admin opens the group in Pollis, then **Bots** → **Add a bot**, and types the code. When the admin confirms, the bot joins the group and `pair` exits.

## Add another host to a bot

On the new machine:

```bash
pollis-bot add-host --bot <bot user id> --name "backup box"
```

An admin opens the group, then **Bots** → the bot → **Add a host**, and types the printed code. One of the bot's running hosts checks the approval and hands over the bot's key. Both hosts then see every message, and for each trigger exactly one of them acts.

## Run

```bash
pollis-bot run                 # every installation on this host, until stopped
pollis-bot run --webhook       # also the alerts webhook on 127.0.0.1:8787
pollis-bot status              # installations and what each is doing
pollis-bot forget --installation <id>   # remove a revoked host's or deleted bot's installation
```

Run it under your service manager (systemd, launchd) so it starts on boot. It needs no network listener of its own except the optional webhook; everything else is outbound to the Delivery Service.

## Alerts

Turn the webhook on in `<data dir>/config.json` (or with `run --webhook`):

```json
{ "webhook": { "bind": "127.0.0.1:8787" } }
```

On the first start with the webhook on, the runtime prints each installation's secret **once**. `pollis-bot rotate-secret --installation <id>` issues a new one.

```bash
curl -X POST http://127.0.0.1:8787/hooks/<installation id> \
  -H "Authorization: Bearer <secret>" \
  -H "Content-Type: application/json" \
  -d '{"channel": "alerts", "text": "disk 91% on db-1"}'

pollis-bot post --installation <id> --channel alerts "deploy finished"
```

- `channel` is a channel id or name in the bot's group; anything else is refused (422).
- Senders that sign can send `X-Pollis-Signature: sha256=<hex HMAC-SHA256(secret, body)>` instead of the bearer header.
- A sender that posts to several hosts of one bot should send the same `X-Pollis-Delivery: <id>` to each; exactly one posts.
- Rate limit, per installation: a burst of 10, then 30 a minute (`webhook.burst`, `webhook.per_minute`). Over it: 429 with `Retry-After`.
- To take alerts from another machine on your network (Grafana on the LAN), set `bind` to that interface. Callers still need the secret.

## Limits

- Installations per runtime: `max_installations` in `config.json`, default **8**. The runtime refuses to start more, and `pair`/`add-host` refuse to add more.
- Bots per group: 25, enforced by the Delivery Service.

## Files

`<data dir>` is `--data-dir`, else `POLLIS_BOT_DATA_DIR`, else `~/.local/share/pollis-bot` (Linux), `~/Library/Application Support/com.pollis.bot` (macOS), `%APPDATA%\pollis-bot` (Windows).

| Path | What |
|---|---|
| `config.json` | `max_installations`, `webhook {bind, per_minute, burst}` |
| `installations/<id>/` | one installation's record |
| `stores/<device id>/` | that installation's encrypted store |
| `status.json` | written by `run` on every state change, read by `status` |

## If something fails

Every failure in the log ends with `Fix:` and one thing to do. The common ones:

| Log says | Do |
|---|---|
| `the code expired before an admin typed it` | Run `pair` / `add-host` again and type the new code before it expires |
| `no installations on this host` | `pollis-bot pair` |
| `configured for at most N installations` | Raise `max_installations`, or run the extra bot on another host |
| `opening installation …` | If an admin revoked this host or deleted the bot, `pollis-bot forget --installation <id>` (then `pair`/`add-host` again if the bot should stay) |
| `job failed while catching up` | Check the machine reaches the Delivery Service; the next message or restart retries |
| `webhook call with a wrong secret or signature` | Give the sender the current secret (`rotate-secret` if lost) |
