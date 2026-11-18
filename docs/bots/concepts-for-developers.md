# Bots: concepts for developers

How the bot runtime works, for anyone who will write code that runs in it. Read the [four terms](./README.md#four-words-four-different-things) first: App, Installation, Bot and Runtime mean different things here.

This page describes how the runtime runs every handler: the built-in alerts handler and Apps, your own code in a sandbox. The [App reference](./app-reference.md) covers what an App can call and declare.

## Contents

- [The shape of it](#the-shape-of-it)
- [How a job runs](#how-a-job-runs)
- [Triggers and trigger keys](#triggers-and-trigger-keys)
- [Why only one host acts](#why-only-one-host-acts)
- [Handlers and actions](#handlers-and-actions)
- [Grants](#grants)
- [Failures and retries](#failures-and-retries)
- [App state](#app-state)
- [Installations are isolated](#installations-are-isolated)
- [What comes next](#what-comes-next)

## The shape of it

- A bot is a member of one group. Its keys live only on its **hosts**, machines running `pollis-bot`.
- On each host, the runtime keeps one **installation** per bot it runs.
- Things that happen (a new message in the group, a webhook call) become **triggers**.
- Each trigger runs through the **job loop**. A **handler** looks at the trigger and returns **actions** (for example, "post this text in #alerts").
- The runtime, not the handler, decides whether each action happens and carries it out.

A handler never holds keys, never talks to the network and never sends anything itself. It only returns a list of what it would like to happen.

## How a job runs

Every trigger, of every kind, goes through the same seven steps:

1. **Trigger.** Something arrives for an installation: the runtime is woken because the group may have new messages, or a webhook call comes in.
2. **Lock.** The runtime takes that installation's lock. One installation runs one job at a time; different installations run in parallel.
3. **Catch up.** The runtime brings the bot's view of the group up to date through the normal message path, the same one the Pollis apps use. Messages newer than the last one handled become message triggers, oldest first. The bot's own messages never become triggers.
4. **Handle.** Each trigger goes to the installation's handler, which returns a list of actions (possibly empty). An App runs in a fresh sandbox for this one trigger and may also have changed its [state](#app-state).
5. **Claim.** If there are actions or state changes, the host claims the trigger on the Delivery Service. If another host of the same bot already claimed it, this host drops everything the handler produced. See [Why only one host acts](#why-only-one-host-acts). A host that wins the claim saves the App's state changes before carrying out any action.
6. **Check and apply.** Each action is checked against the installation's [grants](#grants). Actions that pass are carried out through the normal send path; actions that don't are reported and never carried out.
7. **Unlock.** The lock is released.

There are no timers in this loop. A job starts only when the runtime starts (to catch up on what it missed), when a realtime event says the group may have changed, or when a webhook call arrives. Wake-ups that arrive while a job is queued fold into that job.

## Triggers and trigger keys

Every trigger has a **trigger key**: a string that every host of the bot derives the same way for the same event. The claim in step 5 is made on `(installation id, trigger key)`.

| Trigger | When | Trigger key |
|---|---|---|
| Message | A new message in the bot's group (for an App, a mention of the bot reaches `on-mention`) | `msg:<envelope id>` |
| Webhook | A call to the installation's webhook | `hook:<delivery id>` (the `X-Pollis-Delivery` header, or a fresh id) |
| Schedule | An App's schedule reaches a slot | `slot:<schedule name>@<slot time>` |
| Request | Coming in a later release | `req:<request id>` |

Schedules have no timer loop: after each job the runtime sets one timer to the installation's next slot. A host that was asleep at a slot runs it when it next wakes, up to the three most recent missed slots per schedule (older ones are skipped and logged), and the claim makes each slot run on one host only.

The kind prefix keeps keys from different kinds apart, so a delivery id a sender chooses can never collide with a message's envelope id.

## Why only one host acts

A bot can have several hosts, and each of them sees every message in the group. Without a rule, a bot with three hosts would answer every message three times.

So before carrying out any action, a host **claims** the trigger on the Delivery Service. The claim is a row keyed by `(installation id, trigger key)` with a uniqueness constraint:

- The first host to claim wins and acts.
- Every other host gets "already claimed" and discards its actions.
- If the winner asks again (a retry after a network error), it is told it won again, so it can finish.

The claim is a single request, with no leases and no keepalives. Claims are kept for seven days: a host that retries a trigger within that time gets the same answer, but a trigger first retried more than seven days after it was claimed can be claimed and acted on again.

A trigger with no actions and no state changes is never claimed: there is nothing to deduplicate.

## Handlers and actions

A handler is a function from a trigger to a list of actions:

```text
handle(trigger) -> Ok([action, ...]) | Err(error)
```

There are two handler kinds:

- the built-in **alerts** handler: a webhook call becomes one `post` action with the call's `channel` and `text`; messages in the group are ignored;
- an **App**: a WebAssembly component (written in Rust today, TypeScript with the SDK) that the runtime runs in a sandbox for each trigger. It acts only by calling the functions in the [App reference](./app-reference.md), each checked against its grants before it is even queued.

The actions a handler can return:

| Action | Fields | Rules |
|---|---|---|
| `post` | `channel`, `text` | Text channels of the bot's own group only (the alerts handler may name a channel by id or by name, with or without `#`); text not empty and at most 4,000 characters |
| `reply` | message, `text` | Apps only; a message in a granted channel; same text rules |
| `react` | message, emoji | Apps only; a message in a granted channel |
| `edit_own` | message, `text` | Apps only; a message this bot sent |
| `delete_own` | message | Apps only; a message this bot sent |

There are no actions for deleting other people's messages, removing members or changing roles. Bots never moderate.

If a handler returns an error (for an App: returns `err`, traps, runs out of time or memory), every action of that trigger is discarded, no state change is saved, and nothing is claimed. A handler's work is all or nothing.

## Grants

**Grants** are what an installation is allowed to do. The runtime checks every action against them in step 6, whatever the handler asked for.

For the alerts handler the grant is fixed: `post` to the text channels of the bot's own group. A `post` to any other channel is refused, reported in the log and in the webhook's answer, and never sent.

For an App, the grants are its manifest intersected with what the admin approved: which triggers wake it, which channels and how much history it reads, which actions it may take, how much state it keeps and which hosts it may reach. The sandbox refuses anything outside them, and the job loop checks every action's channel against the group again before carrying it out. Until App approvals ship, a development build of the runtime reads the admin's grants from a local file (`POLLIS_APP_DEV_GRANTS`); release builds cannot.

## Failures and retries

Each message trigger ends in one of these outcomes:

| Outcome | What happened |
|---|---|
| No actions | The handler asked for nothing. |
| Applied | This host won the claim and carried out the allowed actions. |
| Claimed elsewhere | Another host won; this host's actions were discarded. |
| Handler failed | The handler returned an error; nothing was claimed, saved or sent. |

Once a message has one of these outcomes, the runtime moves past it and never handles it again, even after a restart.

If a step fails because the Delivery Service could not be reached (the claim or the send), the job stops at that message and does **not** move past it. The next wake-up retries it, in order, under the same trigger key. The message a job posts has an id derived from the installation, the trigger key and the action's position, so a retry re-sends the same message instead of posting a second one.

## App state

An App's key-value state (`kv-get`, `kv-set`, `kv-delete`) belongs to the installation, not to a host. It is stored on the Delivery Service, sealed with a key that only the bot's hosts can derive (from the bot's own account secret), so:

- every host of the bot reads the same state, and a host woken cold has everything it needs;
- the Delivery Service stores only ciphertext, its size and a version number, never the keys' names or values;
- nothing stays on a host between jobs.

During a job the App sees its own writes immediately, but they are saved only after the host wins the trigger's claim, and only if no other host changed the same keys since the App read them. If one did, the runtime runs the App again on the new state (up to three times), so two hosts never overwrite each other. A retried job never saves the same trigger's changes twice.

## Installations are isolated

One runtime can host many installations (several bots, in one or several groups). Each has:

- its own bot identity and keys,
- its own keystore entry,
- its own encrypted local store,
- its own lock and job loop.

Nothing is shared between two installations: a job works only with its own installation's keys and store. The runtime refuses to host more installations than it is configured for (`max_installations`, default 8).

Today an installation id is the bot's user id, because each bot has exactly one installation.

## What comes next

- **Approval:** an admin approves exactly one App version (pinned by hash) with the grants they chose, and approves again when an update asks for more.
- **More triggers:** slash commands (#2018) and member requests (#2008) reach Apps once the runtime delivers them.
- **History:** `query-messages` returns the trigger's own message until the runtime keeps a message log.
- **SDKs:** a TypeScript SDK and the `pollis app` CLI.

The alerts webhook ([Alerts webhook](./alerts-webhook.md)) works today without any App.
