# Writing a Pollis App in TypeScript

An **App** is the code that decides what a bot does. You write it in TypeScript, test it on your own machine, and build it into a single file that a group's admin approves. You never run the bot yourself and you never hold its keys: the group's own runtime (`pollis-bot`) runs your App inside a sandbox, and the App can do only what its manifest asks for and the admin allowed.

You do not need to know anything about WebAssembly. You write ordinary async TypeScript; the `pollis app` tool does the rest.

If the words App, Installation, Bot and Runtime are new, read [the four terms](./README.md#four-words-four-different-things) first. Every function, field and limit is listed in the [App reference](./app-reference.md); this guide is the walkthrough.

## Contents

- [What works today](#what-works-today)
- [Quick start: zero to a tested App](#quick-start-zero-to-a-tested-app)
- [Your App, line by line](#your-app-line-by-line)
- [The manifest](#the-manifest)
- [Handlers](#handlers)
- [What `ctx` can do](#what-ctx-can-do)
- [Testing](#testing)
- [Building and the App hash](#building-and-the-app-hash)
- [Limits, stated plainly](#limits-stated-plainly)
- [Approval and updates, from the admin's side](#approval-and-updates-from-the-admins-side)
- [Troubleshooting](#troubleshooting)

## What works today

| Command | What it does | Status |
|---|---|---|
| `pollis app new <name> --lang ts` | A new App: one handler, a test and a README | Works |
| `pollis app test` | Builds the App, then runs your tests against it, offline, in the same sandbox the runtime uses | Works |
| `pollis app build` | Writes `dist/app.wasm` and `dist/pollis-app.json`, prints the App hash | Works |
| `pollis app dev` | Runs the App in a local bot against a test group, rebuilding on save | Not available yet: it needs App approval in the Pollis apps |
| `pollis app run` | Runs an approved App for a real group | Not available yet: same reason |

So today you can write an App, test every handler against the real permission checks, and build the exact file an admin will approve. Running it in a group arrives with App approval. Slash commands typed in the Pollis composer arrive alongside it; until then, `onCommand` is reached in tests.

## Quick start: zero to a tested App

You need:

- **Node.js 22.18 or newer** (`node --version`). Your tests are TypeScript, which Node runs directly from 22.18.
- **pnpm** (`npm install -g pnpm`), or npm if you prefer.
- A checkout of the Pollis repository, and **Rust** ([rustup.rs](https://rustup.rs)), to build `pollis-app-dev`: the small program that runs your App in the real sandbox. The SDK packages are not on npm yet, so they come from the checkout too.

**1. Build the tools** (once):

```sh
git clone https://github.com/pollis-app/pollis.git
cd pollis
cargo build --release -p pollis-app-dev
export PATH="$PWD/target/release:$PATH"      # puts pollis-app-dev on your PATH
(cd sdk/ts && pnpm install && pnpm build)    # @pollis/app and @pollis/cli
alias pollis="node $PWD/sdk/ts/cli/dist/main.js"
```

**2. Create an App** next to the checkout:

```sh
cd ..
pollis app new hello-bot --lang ts --sdk pollis/sdk/ts
cd hello-bot
pnpm install
```

`--sdk` points the new App at the SDK in your checkout. Once the packages are on npm you will leave it out.

**3. Run its tests:**

```sh
pnpm test
```

```
built hello-bot 0.1.0 (App hash 71ba9fee…)
✔ /hello greets by name
✔ /hello with no name greets the sender
✔ other commands are ignored
ℹ pass 3
```

Each test ran your built App in the sandbox, with the same permission checks the runtime applies.

**4. Build it:**

```sh
pnpm build
```

```
built hello-bot 0.1.0
  dist/app.wasm         11.6 MB
  dist/pollis-app.json
App hash: 71ba9fee0fbc0dc42d2894bb000f984c00b3776c8a4456d0fdcba1de66f473d7
```

That hash names exactly this build. It is what an admin approves.

## Your App, line by line

`src/app.ts`, as `pollis app new` writes it:

```ts
import { app } from "@pollis/app";

export default app({
  manifest: {
    read: { channels: ["*"] },
    actions: ["reply"],
  },

  async onCommand(cmd, ctx) {
    if (cmd.name !== "hello") {
      return;
    }
    const who = cmd.args.trim() || cmd.message.author.displayName;
    await ctx.reply(cmd.message, `Hello, ${who}!`);
  },
});
```

- `export default app({ ... })` is your whole App: a **manifest** and one or more **handlers**.
- `manifest.read.channels: ["*"]` asks to work in whichever channels the admin picks. `["#support"]` would ask for one channel by name.
- `manifest.actions: ["reply"]` asks to be allowed to reply. Nothing else: this App cannot post on its own, react, or edit anything.
- There is no `egress`, so this App cannot send anything outside the group.
- `onCommand` runs when a member sends a slash command such as `/hello Sam`. `cmd.name` is `"hello"`, `cmd.args` is `"Sam"`, and `cmd.message` is the message that carried it.
- `ctx.reply(...)` queues a reply. It is sent only after the handler finishes without throwing.

## The manifest

The manifest says what your App needs. You write it in code, and `pollis app build` writes it out as `dist/pollis-app.json` beside the component, so it can never drift from what the code expects. Admins see it in plain language before approving, and can narrow it (for example, untick a channel) but never widen it.

| Field | What it means | Default |
|---|---|---|
| `name` | Lowercase letters, digits and `-` | Your package name, without its `@scope/` |
| `version` | Your version, shown to admins | Your package version |
| `description` | Shown to admins | Your package description |
| `triggers` | What wakes the App | One per handler you define |
| `read.channels` | `"#name"` for a channel by name, or `"*"` for whichever the admin grants | None |
| `read.history` | Days of history `ctx.messages()` may read back | 0 |
| `read.events` | `"created"` (new messages); add `"edited"` to also see edits | `["created"]` |
| `actions` | Any of `post`, `reply`, `react`, `edit_own`, `delete_own` | None |
| `state.quota` | Bytes your App may keep in `ctx.state` | 0 (no state) |
| `egress` | Exact host names `ctx.fetch` may reach, e.g. `"api.linear.app"` | None: nothing leaves the group |

A bigger example:

```ts
manifest: {
  read: { channels: ["#support"], history: 7 },
  actions: ["reply", "react"],
  state: { quota: 64 * 1024 },
  egress: ["api.linear.app"],
},
```

Mistakes are caught at build time, with the field named:

```
error: manifest: `egress` entry "https://api.linear.app" is not valid: an exact lowercase host name such as "api.example.com", with no scheme, port, path, wildcard or IP address
error: manifest: `onMessage` would never run: add "messages" to `triggers` (or leave `triggers` out to derive it from your handlers)
```

Ask for the least you need. An admin is more likely to approve a short list, and a later version that asks for more needs fresh approval.

## Handlers

Define any of these. Each gets what happened and `ctx`, and may be `async`.

| Handler | Runs when | Trigger | Gets |
|---|---|---|---|
| `onMessage(message, ctx)` | A new message arrives in a granted channel | `messages` | `Message` |
| `onMention(message, ctx)` | A message mentions the bot | `mentions` | `Message` |
| `onCommand(command, ctx)` | A member sends a slash command | `commands` | `{ name, args, message }` |
| `onSchedule(schedule, ctx)` | A schedule fires | `schedule` | `{ name, slot }` (`slot` in Unix ms) |
| `onRequest(request, ctx)` | A member sends the App a request | `requests` | `{ id, requester, body }` |
| `onWebhook(webhook, ctx)` | An HTTP call reaches the App's webhook | `webhook` | `{ deliveryId, headers, body, text(), json() }` |

A `Message` is `{ id, channel, author: { id, displayName, isBot }, text, attachments, replyTo?, thread?, sentAt, editedAt? }`. Times are Unix milliseconds. `attachments` holds each file's name, type and size, never its bytes.

Your bot's own messages never reach your handlers, so an App cannot loop on what it posted.

**All or nothing.** Everything a handler does (messages, reactions, state writes) is queued and applied only if the handler finishes without throwing. If it throws, or a call it awaits is refused and you do not catch it, nothing is sent and no state is written.

## What `ctx` can do

Every call returns a promise. A call your manifest (or the admin) does not allow rejects with a `RefusedError` whose message names the field to change.

| Call | Does | Needs |
|---|---|---|
| `ctx.reply(message, text)` | Reply to a message | `actions: ["reply"]` |
| `ctx.post(channelId, text)` | Post in a channel (use `message.channel` for the same one) | `"post"` |
| `ctx.react(message, emoji)` | React to a message | `"react"` |
| `ctx.editOwn(message, text)` | Edit a message your bot sent | `"edit_own"` |
| `ctx.deleteOwn(message)` | Delete a message your bot sent | `"delete_own"` |
| `ctx.messages({ channel?, since?, until?, limit? })` | Messages from granted channels, at most `read.history` days old (see below) | `read.channels` |
| `ctx.state.get/getText/getJSON(key)` | Read your App's saved state | `state.quota` above 0 |
| `ctx.state.set/setJSON(key, value)`, `ctx.state.delete(key)` | Change it | `state.quota` above 0 |
| `ctx.fetch(url, { method, headers, body })` | One HTTPS request | the host in `egress` |
| `ctx.log.info(text)` (also `trace`, `debug`, `warn`, `error`) | Write to the runtime's log, never the group | nothing |
| `ctx.now()` | The runtime's clock, Unix ms | nothing |

**`ctx.fetch`** looks like the web's `fetch`:

```ts
const res = await ctx.fetch("https://api.linear.app/graphql", {
  method: "POST",
  headers: { "content-type": "application/json", authorization: "..." },
  body: JSON.stringify({ query }),
});
if (!res.ok) {
  throw new Error(`Linear said ${res.status}`);
}
const data = await res.json<{ data: unknown }>();
```

It reaches only `https://` URLs on hosts listed in `egress`, exactly (`api.linear.app` does not allow `linear.app`). Redirects are followed only within the same host. Responses over 1 MiB are refused.

**`ctx.messages`** returns messages from granted channels only, never older than `read.history` days (0: only the message that triggered the job). In a group, until the runtime keeps a message log, it returns only the message that triggered the job; in tests it searches the `messages` you give.

**State** is a small key-value store for your installation, sealed so only the bot's hosts can read it. It is shared by every host that runs the bot, and survives between jobs; nothing else does (every job starts with a fresh copy of your App, so module-level variables are reset).

```ts
const count = ((await ctx.state.getJSON<number>("count")) ?? 0) + 1;
await ctx.state.setJSON("count", count);
```

**Handling a refusal** is ordinary error handling:

```ts
import { RefusedError } from "@pollis/app";

try {
  await ctx.fetch("https://status.example.com/");
} catch (e) {
  if (e instanceof RefusedError) {
    ctx.log.warn(e.message);   // "`http-fetch` to `status.example.com` refused: not in `egress`"
  }
}
```

`console.log` works too: it goes to the runtime's log at debug level.

## Testing

`pollis app test` builds your App, then runs every `*.test.ts` under `src/` and `test/` with Node's test runner. In a test, `testApp()` gives you your built App:

```ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { testApp } from "@pollis/app/testing";

const app = await testApp();

test("/hello greets by name", async () => {
  const result = await app.command("hello", "Sam");
  assert.ok(result.ok, result.error?.message);
  assert.deepEqual(result.replies, ["Hello, Sam!"]);
});
```

Each call runs one handler in the real sandbox, with the real permission checks: a call your manifest does not allow fails in a test with the same message it would in a group.

| Call | Runs |
|---|---|
| `app.message(text or { text, channel, author, ... })` | `onMessage` |
| `app.mention(...)` | `onMention` |
| `app.command(name, args?, { message? })` | `onCommand` |
| `app.schedule(name, { slot? })` | `onSchedule` |
| `app.request(body, { requester? })` | `onRequest` |
| `app.webhook(body, { headers? })` | `onWebhook` |

What you get back:

| Field | Holds |
|---|---|
| `ok` | The handler finished without throwing |
| `error` | `{ kind, message }` when it did not: `handler_failed`, `not_delivered` (the trigger was outside the grants), `timeout`, `trap`... |
| `replies`, `posts`, `reactions` | What the App sent, by kind |
| `actions` | Everything it queued, in order |
| `state` | State it wrote, as text (`null`: deleted) |
| `logs` | Its `ctx.log` and `console.log` lines |
| `fetches` | Every `ctx.fetch` that passed the egress check |

When a job fails, `actions` and `state` are empty, because nothing would have been applied.

Every call also takes options, which you can give once to `testApp(...)` as defaults:

```ts
const result = await app.command("ticket", "Printer on fire", {
  // Answers for ctx.fetch. Tests never touch the network; an unanswered request fails.
  http: [{ url: "https://api.linear.app/graphql", method: "POST", body: { data: { id: "T-7" } } }],
  // State before the job.
  state: { count: "41" },
  // History ctx.messages() can find.
  messages: [{ text: "earlier", channel: "support", sentAt: Date.UTC(2027, 0, 15, 7) }],
  // Test a narrower installation than the manifest asks for.
  grants: { channels: { support: "support" }, actions: ["reply"], egress: [] },
  // The runtime's clock. Default: 2027-01-15T08:00:00Z.
  now: Date.UTC(2027, 0, 15, 8),
});
```

In tests a channel's id is its name without `#`: `#support` is `"support"`. Messages land in your manifest's first named channel unless you say otherwise.

## Building and the App hash

`pollis app build`:

1. reads your manifest from the code and checks it;
2. bundles your App and its npm dependencies into one file;
3. compiles that into a WebAssembly component (with ComponentizeJS, which embeds the StarlingMonkey JavaScript engine);
4. loads the result in the real sandbox, so an App the runtime would refuse is refused now;
5. prints the **App hash**: SHA-256 over the component and the manifest.

The build is reproducible: the same source and the same lockfile (`pnpm-lock.yaml`) give the same bytes and the same App hash on any machine, so anyone can rebuild your App from source and check that it is the one an admin approved. Commit your lockfile.

The component is about 12 MB, almost all of it the JavaScript engine, whatever the size of your code.

The entry is `src/app.ts`. To use another file, set it in `package.json`:

```json
{ "pollis": { "entry": "src/bot.ts" } }
```

## Limits, stated plainly

- **An App can send whatever it reads to the hosts in its `egress`.** The sandbox stops it reaching any other host, but it cannot know what the App does with data on the hosts it is allowed. That is why the egress list is part of what an admin approves, and why an App with no egress can say it cannot send data outside the group.
- **No Node built-ins.** `fs`, `net`, `http`, `child_process` and the rest are refused at build, with the reason. Apps have no files, sockets or processes. Use `ctx.state` to keep data and `ctx.fetch` to reach the network.
- **npm packages** work when they are plain JavaScript. The build picks a package's browser version when it has one. A package that needs Node built-ins fails to build, naming the import.
- **No attachment bytes.** You see each attachment's name, type and size.
- **Every job is fresh.** Nothing in memory survives from one job to the next. Use `ctx.state`.
- **Per-job limits**: 10 seconds, 64 MiB of memory, 16 HTTP requests, 32 queued actions, 1 MiB per HTTP response. Going over ends the job and nothing is applied. The full table is in the [App reference](./app-reference.md#per-job-limits).
- **No moderation.** No App can delete other people's messages, remove members or change roles.
- **The sandbox engine is trusted.** The runtime's WebAssembly engine (Wasmtime) enforces all of the above; a flaw in it would be a flaw in the sandbox.

## Approval and updates, from the admin's side

This is how it will work once App approval ships:

- An admin adds your App to their group by its App hash. They see your manifest in plain language ("Reads #support, last 7 days", "Can send data to: api.linear.app"), can narrow it, and approve.
- The runtime runs exactly that hash and nothing else. A build whose hash differs, even by one byte, does not run.
- Every new version is a new hash and needs a new approval. If it asks for more than the approved one (a new channel, a longer history, a new action, a new host), the admin sees that highlighted.

## Troubleshooting

**`pollis-app-dev` was not found.** Build it (`cargo build --release -p pollis-app-dev` in the Pollis checkout) and put `target/release` on your `PATH`, or set `POLLIS_APP_DEV_BIN` to the binary.

**`another pollis app build is running`.** Builds take turns on one machine; run yours when the other finishes.

**A test result says `not_delivered`.** The trigger was outside what the App was granted: a channel not in `read.channels`, an edited message without `"edited"` in `read.events`, or a handler whose trigger is not in `triggers`.

**`the App host refuses this build`.** The component imports something the sandbox does not provide. The message names the import; this usually means a dependency reaches for the network or files directly.

**Tests fail with a TypeScript syntax error.** Your Node is older than 22.18. Check `node --version`.
