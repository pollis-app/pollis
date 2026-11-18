# Pollis bots

A Pollis bot is a member of one group that is run by a program, not a person.

- It runs on a machine that one of the group's admins controls, never on Pollis's servers.
- It is a real member: it decrypts the group's messages and posts into its channels with its own keys.
- It belongs to exactly one group, for its whole life, and is managed by that group's admins.
- Members see it in the member list and on its messages with a **Bot** badge.
- No developer and no Pollis server holds its keys.

## Contents

| Page | For | What it covers |
|---|---|---|
| [Admin guide](./admin-guide.md) | Group admins | What a bot can read, adding a bot, adding and revoking hosts, deleting a bot, limits |
| [Self-hosting](./self-hosting.md) | Whoever runs the bot's machine | Installing `pollis-bot`, pairing, running it as a service, logs, troubleshooting |
| [Alerts webhook](./alerts-webhook.md) | Developers and operators | Posting alerts into a channel with `curl`, Grafana or `pollis-bot post` |
| [Concepts for developers](./concepts-for-developers.md) | Developers | How a job runs, triggers and trigger keys, why only one host acts, grants |

Internals (schema, endpoints, protocol details) live in the contributor wiki: [`.codesight/wiki/bots.md`](../../.codesight/wiki/bots.md). The runtime's own reference is [`pollis-bot/README.md`](../../pollis-bot/README.md).

## Four words, four different things

These words are kept apart everywhere in Pollis: in the apps, in these docs and in the code.

| Word | What it is |
|---|---|
| **App** | Code that decides what a bot does. Written by anyone, run only inside the runtime. |
| **Installation** | One App set up in one group, with the permissions (grants) an admin chose. |
| **Bot** | The installation's user: the member everyone in the group sees. |
| **Runtime** | Pollis's `pollis-bot` program. The only thing that ever holds a bot's keys or talks to the network for it. |

A bot is not the machine it runs on, and not the code it runs. Machines (hosts) can be added and removed, and the code can change, while the bot stays the same member.

Today every bot has one installation, and that installation runs the runtime's built-in **alerts** handler: it posts the alerts it receives on its webhook into a channel. Writing your own App is coming in a later release; [Concepts for developers](./concepts-for-developers.md) describes the job loop it will plug into.

## Quick start

1. An admin opens the group, then **Bots** → **Add a bot**.
2. On the machine that will run the bot, run `pollis-bot pair` ([Self-hosting](./self-hosting.md#install)).
3. Type the eight-character code it prints into the **Add a bot** page, give the bot a name, and confirm.
4. On the machine, run `pollis-bot run --webhook` and note the webhook secret it prints once.
5. Send an alert:

   ```bash
   curl -X POST http://127.0.0.1:8787/hooks/<installation id> \
     -H "Authorization: Bearer <secret>" \
     -H "Content-Type: application/json" \
     -d '{"channel": "general", "text": "Hello from my bot"}'
   ```

The installation id is shown by `pollis-bot status`.
