# Bots: admin guide

For group admins. No technical background needed, except for one command that whoever runs the bot's machine types (see [Self-hosting](./self-hosting.md)).

Everything here works the same in the desktop app and the mobile app. Only the way to the **Bots** page differs:

- **Desktop:** open the group, then choose **Bots**.
- **Mobile:** open the group's settings, then **Bots**.

Only the group's admins see the **Bots** page.

## Contents

- [What a bot is](#what-a-bot-is)
- [What a bot can read](#what-a-bot-can-read)
- [Add a bot](#add-a-bot)
- [Add a host](#add-a-host)
- [Revoke a host](#revoke-a-host)
- [Delete a bot](#delete-a-bot)
- [If every host is lost](#if-every-host-is-lost)
- [Limits](#limits)
- [The Bot badge](#the-bot-badge)
- [Approving an App](./app-approval.md) (its own page)

## What a bot is

A bot is a member of your group that is run by a program instead of a person. It has its own name and its own keys, like any member.

- It belongs to **one group only**, for its whole life.
- Any admin of the group can add it, change its hosts and delete it.
- It runs on a machine an admin controls, called a **host**. A bot can have several hosts, so it keeps running when one machine goes down.
- It is not in user search. Nobody can send it a direct message, and it cannot send one.
- It can never be an admin or the owner of the group, and it cannot join other groups.

## What a bot can read

A bot is a full member of its group, so:

- It can read **every message sent in the group's channels after it joined**, the same as a person who joined at that moment.
- It cannot read messages sent before it joined, your direct messages, or other groups.
- Whoever controls a host machine can read what the bot reads. Run a bot only on a machine you trust with that.

A bot runs an **App** an admin approved, and does nothing until it has one. The [alerts App](../../apps/alerts/README.md), for example, posts the alerts it receives on its webhook ([Alerts webhook](./alerts-webhook.md)). An App can read and do only what its approval allows, and runs only the exact version that was approved: see [Approving an App](./app-approval.md).

## Add a bot

You need someone at the machine that will run the bot. That can be you.

1. On the **Bots** page, choose **Add a bot**. The page shows the command to run on the machine.
2. On the machine, install `pollis-bot` ([Self-hosting → Install](./self-hosting.md#install)) and run:

   ```bash
   pollis-bot pair
   ```

   It prints an eight-character **pairing code** and the time it expires (ten minutes later).
3. Back on the **Add a bot** page, type a **bot name** (members see it on the bot's messages) and the code. Type the code yourself; it is never filled in for you, because typing it is how you confirm you are adding the machine in front of you.
4. Choose **Add bot**.

The bot joins the group, and `pollis-bot pair` on the machine finishes. Whoever runs the machine then starts it with `pollis-bot run` ([Self-hosting → Run](./self-hosting.md#run)).

If the page says the code doesn't match, read it again from the machine and retype it. If it says the code has expired, run `pollis-bot pair` again for a new one.

## Add a host

A second host keeps the bot running when one machine is down. Each host holds the bot's keys.

1. Open the bot from the **Bots** page and choose **Add a host**. The page shows the command for the new machine, with the bot's id filled in.
2. On the new machine, install `pollis-bot` and run that command:

   ```bash
   pollis-bot add-host --bot <bot id>
   ```

   It prints a pairing code.
3. Type the code on the **Add a host** page and choose **Add host**.

One of the bot's running hosts checks your approval and hands the new machine the bot's keys. At least one existing host must be running for this to finish. Then start the new host with `pollis-bot run`.

When a bot has several hosts, all of them see every message, and for each alert exactly one of them posts.

## Revoke a host

Revoke a host when the machine is retired, lost or no longer trusted.

1. Open the bot from the **Bots** page.
2. Next to the host, choose **Revoke**, then confirm.

That machine stops running the bot and cannot read the group's new messages. The bot's other hosts keep it running.

If it is the bot's **only** host, the page warns you first: after revoking it, the bot can never run again (see [If every host is lost](#if-every-host-is-lost)).

## Delete a bot

1. Open the bot from the **Bots** page.
2. Choose **Delete bot**, then confirm.

The bot leaves the group and stops running on every host. This cannot be undone.

Deleting a group deletes its bots too.

## If every host is lost

A bot's keys exist only on its hosts. Pollis keeps no copy and has no recovery for them, so that nobody but the bot's own machines can ever act as the bot.

If every host is gone (machines wiped, revoked or lost), the bot cannot be recovered. Delete it and add a new one. The **Bots** page says this before you revoke a last host, and shows it on a bot with no hosts left.

To avoid this, give an important bot a second host.

## Limits

- **25 bots per group.** When a group has 25, the **Bots** page says so; delete one to add another.
- **Bots per machine:** set by whoever runs it (default 8). See [Self-hosting → Several bots on one machine](./self-hosting.md#several-bots-on-one-machine).

## The Bot badge

Everywhere members see a bot (the member list, its messages, and when someone mentions it) its name carries a **Bot** badge. The bot's profile, and its row in the member list on mobile, add "Managed by the admins of <group>". On desktop the same words show when you hover the badge.

Apps and the terminal client that predate bots show a bot as an ordinary member, without the badge.

A bot running a development build also carries a **Development build** mark, and a bot whose App may reach no internet address carries **Can't send data outside this group**. Every member sees both. See [Approving an App](./app-approval.md).
