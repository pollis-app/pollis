# Bots: alerts webhook

Post alerts from your monitoring, CI or scripts into a channel of your group. Each bot's runtime listens for them on a small HTTP endpoint on its own machine; nothing goes through a Pollis server in plain text. The runtime encrypts the alert and posts it as the bot.

## Contents

- [Before you start](#before-you-start)
- [The endpoint](#the-endpoint)
- [Authenticate](#authenticate)
- [Send from curl](#send-from-curl)
- [Send from Grafana](#send-from-grafana)
- [Send from the command line: pollis-bot post](#send-from-the-command-line-pollis-bot-post)
- [Several hosts: send once, post once](#several-hosts-send-once-post-once)
- [Rate limit](#rate-limit)
- [Answers](#answers)
- [Limits](#limits)

## Before you start

1. A bot is paired and running ([Self-hosting](./self-hosting.md)).
2. The webhook is on: either `pollis-bot run --webhook`, or in `config.json` in the data directory:

   ```json
   { "webhook": { "bind": "127.0.0.1:8787" } }
   ```

   The webhook is off until you do one of these.
3. You have the installation's **secret**. `pollis-bot run` prints it once, the first time it starts with the webhook on:

   ```text
   Webhook secret for installation 01JD… (shown once; give it to your alert sender):
       pbw_…
   ```

   Lost it? `pollis-bot rotate-secret --installation <id>` prints a new one once. Restart `pollis-bot run` for the new secret to take effect; the old one then stops working.
4. You have the **installation id**: the first column of `pollis-bot status`.

## The endpoint

```text
POST http://<bind address>/hooks/<installation id>
Content-Type: application/json

{"channel": "alerts", "text": "disk 91% on db-1"}
```

- `channel`: a text channel of the bot's group, by **name** (with or without `#`, any case) or by **id**. If two channels share the name, use the id.
- `text`: the message, up to 4,000 characters.

By default the listener binds to `127.0.0.1:8787`, so only programs on the same machine can reach it. To take alerts from another machine on your network (for example Grafana on your LAN), set `bind` to that interface's address, such as `"192.168.1.20:8787"`, or `"0.0.0.0:8787"` for every interface. Callers still need the secret. The listener speaks plain HTTP; keep it on networks you trust, or put a TLS reverse proxy in front of it.

## Authenticate

Send the secret in **one** of two ways:

| Header | Value |
|---|---|
| `Authorization` | `Bearer <secret>` |
| `X-Pollis-Signature` | `sha256=<hex HMAC-SHA256 of the raw request body, keyed with the secret>` |

Use the bearer header unless your sender signs requests. If a request carries `X-Pollis-Signature`, the signature must be right: a bearer header cannot make up for a wrong one.

## Send from curl

Set these once:

```bash
SECRET='pbw_…'                 # from `pollis-bot run`
INSTALLATION='01JD…'           # from `pollis-bot status`
URL="http://127.0.0.1:8787/hooks/$INSTALLATION"
```

With the bearer header:

```bash
curl -sS -X POST "$URL" \
  -H "Authorization: Bearer $SECRET" \
  -H "Content-Type: application/json" \
  -d '{"channel": "alerts", "text": "disk 91% on db-1"}'
```

With a signature:

```bash
BODY='{"channel": "alerts", "text": "deploy finished"}'
SIG=$(printf '%s' "$BODY" | openssl dgst -sha256 -hmac "$SECRET" | sed 's/^.* //')
curl -sS -X POST "$URL" \
  -H "X-Pollis-Signature: sha256=$SIG" \
  -H "Content-Type: application/json" \
  --data-binary "$BODY"
```

Sign exactly the bytes you send (`--data-binary`, not `-d`, which can alter the body).

A successful post answers:

```json
{"status": "posted", "delivery_id": "01JD…", "posted": 1, "refused": []}
```

## Send from Grafana

In Grafana, create a contact point of type **Webhook**:

| Field | Value |
|---|---|
| URL | `http://<bot machine>:8787/hooks/<installation id>` |
| HTTP Method | `POST` |
| Authorization Header - Scheme | `Bearer` |
| Authorization Header - Credentials | the secret |
| Custom Payload | the template below |

```text
{"channel": "alerts", "text": "[{{ .Status }}] {{ .CommonLabels.alertname }}"}
```

The bot's runtime needs `bind` set to an address Grafana can reach (see [The endpoint](#the-endpoint)). The payload must be JSON with `channel` and `text`; Grafana's default webhook body has neither, so the custom payload is required.

TODO(#1182): verify this contact point against a live Grafana and record the minimum Grafana version that offers Custom Payload.

## Send from the command line: pollis-bot post

On the bot's machine, `pollis-bot post` sends through the running runtime's own webhook, signed with the installation's secret, so you don't handle the secret yourself:

```bash
pollis-bot post --installation 01JD… --channel alerts "backup finished"
```

It needs `pollis-bot run` to be running with the webhook on. It takes exactly the same path as any other sender: secret check, rate limit, job loop, grants.

## Several hosts: send once, post once

When a bot has several hosts, you can send the same alert to each of them, so it still arrives when one is down. Add the same `X-Pollis-Delivery` header to every copy:

```bash
for host in bot-a.lan bot-b.lan; do
  curl -sS -X POST "http://$host:8787/hooks/$INSTALLATION" \
    -H "Authorization: Bearer $SECRET" \
    -H "Content-Type: application/json" \
    -H "X-Pollis-Delivery: disk-db-1-2026-11-18T09:15" \
    -d '{"channel": "alerts", "text": "disk 91% on db-1"}'
done
```

Exactly one host posts it; the others answer `claimed_elsewhere`. The delivery id is 1 to 128 characters of `A-Z a-z 0-9 . _ : -`. Without the header, every call is a new delivery.

The same header makes retries safe: resending a delivery id the same day does not post it twice.

## Rate limit

Per installation: a burst of **10** calls, refilled at **30 calls a minute**. A call over the limit gets `429 Too Many Requests` with a `Retry-After` header (seconds), and nothing is posted.

Change it in `config.json`:

```json
{ "webhook": { "bind": "127.0.0.1:8787", "per_minute": 60, "burst": 20 } }
```

## Answers

Every answer is JSON with a `status` field. Checks run in this order: installation, secret, rate limit, body.

| HTTP | `status` | Meaning | Do this |
|---|---|---|---|
| 200 | `posted` | Posted. `refused` lists anything not posted. | Nothing. |
| 200 | `claimed_elsewhere` | Another host of the bot posted this delivery. | Nothing. |
| 202 | `accepted` | The job took over 30 seconds; it still finishes. | Nothing; check the bot's log if it never appears. |
| 400 | `bad_request` | The body is not `{"channel", "text"}` JSON, or `X-Pollis-Delivery` is malformed. | Read `fix` in the answer. |
| 401 | `unauthorized` | Wrong or missing secret or signature. | Use the current secret. |
| 404 | `unknown_installation` | No such installation on this machine. | Use an id from `pollis-bot status`. |
| 413 | | The body is over 64 KiB. | Send less. |
| 422 | `refused` | The channel is not a text channel of the bot's group, the text is empty, or it is over 4,000 characters. `refused` says which. | Fix the channel or the text. |
| 429 | `rate_limited` | Over the rate limit. | Wait `Retry-After` seconds. |
| 500 / 503 | `failed` | The job could not finish (for example, the Delivery Service was unreachable). `fix` says what to do. | Retry with the same `X-Pollis-Delivery`. |

## Limits

| Limit | Value |
|---|---|
| Text per post | 4,000 characters |
| Request body | 64 KiB |
| Rate, per installation | burst 10, then 30 a minute (configurable) |
| Where it posts | text channels of the bot's own group only |
