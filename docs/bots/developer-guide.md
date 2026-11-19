# Writing a Pollis App

An **App** is the code that decides what a bot does. You write it in TypeScript or Rust, test it on your own machine, and build it into a single file that a group's admin approves. Your App never sees the bot's keys: the runtime (`pollis-bot`, on a machine the group's admin chooses) holds them and runs your App inside a sandbox, where it can do only what its manifest asks for and the admin allowed.

You do not need to know anything about WebAssembly. You write ordinary TypeScript or Rust; the `pollis app` tool does the rest. This guide walks through TypeScript. Everything about manifests, handlers, limits and approval is the same in Rust, and [Writing your App in Rust](#writing-your-app-in-rust) shows what differs.

If the words App, Installation, Bot and Runtime are new, read [the four terms](./README.md#four-words-four-different-things) first. Every function, field and limit is listed in the [App reference](./app-reference.md); this guide is the walkthrough.

## Contents

- [The commands](#the-commands)
- [Quick start: zero to a tested App](#quick-start-zero-to-a-tested-app)
- [Your App, line by line](#your-app-line-by-line)
- [The manifest](#the-manifest)
- [Handlers](#handlers)
- [What `ctx` can do](#what-ctx-can-do)
- [Testing](#testing)
- [Building and the App hash](#building-and-the-app-hash)
- [Limits, stated plainly](#limits-stated-plainly)
- [Running it in a group: dev, build, run, approve](#running-it-in-a-group-dev-build-run-approve)
- [Updates, from the admin's side](#updates-from-the-admins-side)
- [Writing your App in Rust](#writing-your-app-in-rust)
- [Troubleshooting](#troubleshooting)

## The commands

| Command | What it does |
|---|---|
| `pollis app new <name> --lang ts` (or `--lang rust`) | A new App: one handler, a test and a README |
| `pollis app test` | Builds the App, then runs your tests against it, offline, in the same sandbox the runtime uses |
| `pollis app build` | Writes `dist/app.wasm` and `dist/pollis-app.json`, prints the App hash |
| `pollis app dev --installation <id>` | Runs the App in your bot against a test group as a **development installation**, rebuilding and signing on every save |
| `pollis app run --installation <id>` | Hands the build to your bot and runs it for a real group, once an admin approves its App hash |

`new`, `test` and `build` need nothing but your machine. `dev` and `run` need a bot: `pollis-bot`, paired with a group ([Self-hosting](./self-hosting.md)).

## Quick start: zero to a tested App

You need:

- **Node.js 22.18 or newer** (`node --version`). Your tests are TypeScript, which Node runs directly from 22.18.
- **pnpm** (`npm install -g pnpm`), or npm if you prefer.
- A checkout of the Pollis repository, and **Rust** ([rustup.rs](https://rustup.rs)), to build `pollis-app-dev`: the small program that runs your App in the real sandbox.
- Later, to run the App in a group: `pollis-bot`, paired with that group ([Self-hosting](./self-hosting.md)).

**1. Build the tools** (once):

```sh
git clone https://github.com/pollis-app/pollis.git
cd pollis
cargo build --release -p pollis-app-dev
export PATH="$PWD/target/release:$PATH"      # puts pollis-app-dev on your PATH
```

**2. Create an App** in a new folder (anywhere; not inside the checkout):

```sh
npx pollis app new hello-bot --lang ts
cd hello-bot
pnpm install
```

The App depends on `@pollis-inc/app` and, for the `pollis` command, `@pollis-inc/cli`. Inside it, `pnpm test`, `pnpm build` and `pnpm pollis app <command>` use that copy. To work against the SDK in your checkout instead, add `--sdk <checkout>/sdk/ts`.

**3. Run its tests:**

```sh
pnpm test
```

```
built hello-bot 0.1.0 (App hash 71ba9fee…)
✔ /hello greets by name
✔ /hello with no name greets the sender
✔ a command the App did not declare never reaches it
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
import { app } from "@pollis-inc/app";

export default app({
  manifest: {
    read: { channels: ["*"] },
    actions: ["reply"],
    commands: [{ name: "hello", description: "Say hello", arg_hint: "[name]" }],
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
- `manifest.commands` declares `/hello`. Members see it when they type `/` in the composer, with its description and the greyed hint `[name]`. Only declared commands ever reach the App.
- `onCommand` runs when a member sends `/hello Sam`. `cmd.name` is `"hello"`, `cmd.args` is `"Sam"`, and `cmd.message` is the message that carried it.
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
| `commands` | Slash commands: `{ name, description, arg_hint?, request? }`, up to 25 | None |
| `schedules` | When `onSchedule` runs: `{ name, every, offset? }` in seconds | None |

**Commands.** Each has a `name` (lowercase letters, digits, `-` and `_`, starting with a letter, up to 32 characters, unique), a one-line `description` (up to 100 characters) and, if it takes arguments, an `arg_hint` (up to 60 characters, e.g. `"<title>"`). `onCommand` needs at least one command. A command with `request: true` is delivered privately to `onRequest` instead of being posted in the channel.

**Schedules.** Each fires at every Unix time `k × every + offset` seconds (UTC), so `{ name: "daily", every: 86400, offset: 32400 }` fires at 09:00 UTC every day. `every` is 60 (a minute) to 604800 (a week); `offset` is less than `every`. Names are lowercase letters, digits and `-`, unique. `onSchedule` needs at least one.

A bigger example:

```ts
manifest: {
  read: { channels: ["#support"], history: 7 },
  actions: ["reply", "react"],
  state: { quota: 64 * 1024 },
  egress: ["api.linear.app"],
  commands: [{ name: "bug", description: "File a bug in Linear", arg_hint: "<title>" }],
  schedules: [{ name: "digest", every: 86400, offset: 32400 }],
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
| `onMention(message, ctx)` | A message @mentions the bot by its username | `mentions` | `Message` |
| `onCommand(command, ctx)` | A member sends one of your `commands` | `commands` | `{ name, args, message }` |
| `onSchedule(schedule, ctx)` | One of your `schedules` fires | `schedule` | `{ name, slot }` (`slot` in Unix ms) |
| `onRequest(request, ctx)` | A member sends the App a request (e.g. a `request: true` command) | `requests` | `{ id, requester, body }` |
| `onWebhook(webhook, ctx)` | An HTTP call reaches the App's webhook | `webhook` | `{ deliveryId, headers, body, text(), json() }` |

Each message reaches **one** handler, chosen by the runtime, never by your App: a declared command goes to `onCommand`; otherwise, a message that mentions the bot goes to `onMention` (if you define it); otherwise `onMessage` (if you define it).

- **Commands.** `/bug Printer on fire` gives `{ name: "bug", args: "Printer on fire" }`. When two Apps in a group declare the same command, the composer adds the bot's name (`/bug @linear_bot Printer on fire`); `args` never includes it. `args` is trimmed.
- **Mentions.** `@` plus the bot's username, at the start of the message or after a space, any case, trailing punctuation ignored. `@all` is not a mention of the bot.
- **Webhooks.** You get the request body exactly as sent, and its `content-type`, `user-agent` and `x-*` headers. Never `authorization`, cookies or the signature.

A `Message` is `{ id, channel, author: { id, displayName, isBot }, text, attachments, replyTo?, thread?, sentAt, editedAt? }`. Times are Unix milliseconds. `attachments` holds each file's name, type and size, never its bytes.

Your bot's own messages never reach your handlers, so an App cannot loop on what it posted.

**All or nothing.** Everything a handler does (messages, reactions, state writes) is queued and applied only if the handler finishes without throwing. If it throws, or a call it awaits is refused and you do not catch it, nothing is sent and no state is written.

## What `ctx` can do

Every call returns a promise. A call your manifest (or the admin) does not allow rejects with a `RefusedError` whose message names the field to change.

| Call | Does | Needs |
|---|---|---|
| `ctx.reply(message, text)` | Reply to a message | `actions: ["reply"]` |
| `ctx.post(channel, text)` | Post in a granted channel, by id (`message.channel` for the same one) or by name (`"support"` or `"#support"`, any case) | `"post"` |
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
import { RefusedError } from "@pollis-inc/app";

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
import { testApp } from "@pollis-inc/app/testing";

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
| `app.send(text or { text, channel, author, ... })` | Whichever handler the runtime would pick: a declared command, else a mention, else a message |
| `app.command(name, args?, { message? })` | A member sending `/name args`, routed like `send`: an undeclared command never reaches `onCommand` |
| `app.message(...)` | `onMessage`, directly |
| `app.mention(...)` | `onMention`, directly |
| `app.schedule(name, { slot? })` | `onSchedule` for a declared schedule; default slot: the latest at or before `now` |
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

In tests a channel's id is its name without `#`: `#support` is `"support"`. Messages land in your manifest's first named channel unless you say otherwise. The bot's username is `test_bot` (`testApp({ botUsername })` to change it), so `app.send("hey @test_bot")` is a mention.

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

## Running it in a group: dev, build, run, approve

You need a bot paired with the group, on your machine ([Self-hosting](./self-hosting.md): `pollis-bot pair`, then an admin adds it on the group's **Bots** page). `pollis-bot status` shows its **installation id**.

**While you develop: a test group and `pollis app dev`.**

```sh
pnpm pollis app dev --installation <installation id>
```

1. It prints your **developer key**, created the first time and kept in the bot's data directory.
2. In the test group, an admin opens the bot's page, turns on **Development installation** and approves that key. This happens once.
3. `pollis app dev` builds the App, signs the build with your key, hands it to the bot and runs the bot. Send `/hello` in the test group: the bot answers.
4. Save a file under `src/`: it rebuilds and signs again, and the bot's next job runs the new build. A build that fails to compile leaves the last good one running.

A development installation runs any build signed by your key, as long as the build asks for **no more** than what the admin approved; a build that asks for more (a new channel, action, host or command) waits for the admin. Every member sees a **Development build** mark on the bot while it is on. Use it in test groups only.

**For real: `pollis app build`, `pollis app run`, approve.**

```sh
pnpm pollis app build
pnpm pollis app run --installation <installation id>
```

`run` builds the App (or takes `--artifact <dir or https:// URL>`, e.g. a release you published), hands it to the bot with `pollis-bot app install`, which prints the **App hash**, and runs the bot. An admin of the group opens the bot's page, checks the hash and the permissions in plain language, narrows them if they like, and approves. The bot runs that exact build and nothing else; until the approval arrives, it waits.

To keep it running after you close the terminal, run `pollis-bot` as a service ([Self-hosting → Run as a service](./self-hosting.md#run-as-a-service)); `pollis app run` is the same as `pollis-bot app install` followed by `pollis-bot run`. `--data-dir` passes the bot's data directory through if it is not the default.

## Updates, from the admin's side

- Every new version is a new App hash and needs a new approval. If it asks for more than the approved one (a new channel, a longer history, a new action, a new host, a new command), the admin sees that highlighted and nothing is pre-accepted.
- A version that asks for no more can be approved with one tap.
- The bot runs exactly the approved hash. A build whose hash differs, even by one byte, does not run, and the bot's page says why.

The admin's side in full: [Approving an App](./app-approval.md).

## Writing your App in Rust

The same App, the same sandbox, the same commands. A Rust App is a small crate that depends on `pollis-app`, and it builds to the same two files: `dist/app.wasm` and `dist/pollis-app.json`. Its component is far smaller than a TypeScript one (about 50 KB for the template), because it carries no JavaScript engine.

**You need** [Rust through rustup](https://rustup.rs) and `pollis-app-dev` on your `PATH` ([step 1 above](#quick-start-zero-to-a-tested-app)). Nothing else: no extra Cargo tools. The App's `rust-toolchain.toml` tells rustup which compiler and WebAssembly target to fetch.

**Create it and run its tests:**

```sh
npx pollis app new hello-bot --lang rust
cd hello-bot
npx pollis app test
```

```
built hello-bot 0.1.0 (App hash bceb6fda…)
test a_command_the_app_did_not_declare_never_reaches_it ... ok
test hello_greets_by_name ... ok
test hello_with_no_name_greets_the_sender ... ok
test result: ok. 3 passed; 0 failed
```

To work against the SDK in your checkout instead of the published crate, add `--sdk <checkout>/sdk/ts`. It copies the crate into your App as `vendor/pollis-app`, so the App hash does not depend on where your checkout is.

**`src/lib.rs`, as `pollis app new` writes it:**

```rust
use pollis_app::{Action, Command, CommandSpec, Context, Manifest};

fn on_command(cmd: Command, ctx: &Context) -> pollis_app::Result {
    if cmd.name != "hello" {
        return Ok(());
    }
    let who = match cmd.args.trim() {
        "" => cmd.message.author.display_name.as_str(),
        name => name,
    };
    ctx.reply(&cmd.message, format!("Hello, {who}!"))
}

pollis_app::app! {
    manifest: Manifest::new()
        .channels(["*"])
        .actions([Action::Reply])
        .command(CommandSpec::new("hello", "Say hello").arg_hint("[name]")),
    on_command: on_command,
}
```

- `pollis_app::app! { ... }` is your whole App: a **manifest**, then the handlers you have. Call it once, in `src/lib.rs`.
- A handler is a function taking what happened and `&Context`. It returns `pollis_app::Result`: `Ok(())` applies what it queued, an error discards all of it. `?` works on every `ctx` call and on any standard error.
- `bail!("no ticket {id}")` returns an error from a handler.

**The manifest** has the same fields as [above](#the-manifest), as builder methods. Name, version and description default to your `Cargo.toml`.

| TypeScript | Rust |
|---|---|
| `read: { channels: ["#support"], history: 7 }` | `.channels(["#support"]).history(7)` |
| `read: { events: ["created", "edited"] }` | `.events([EventKind::Created, EventKind::Edited])` |
| `actions: ["reply", "edit_own"]` | `.actions([Action::Reply, Action::EditOwn])` |
| `state: { quota: 1024 }` | `.state_quota(1024)` |
| `egress: ["api.linear.app"]` | `.egress(["api.linear.app"])` |
| `commands: [{ name, description, arg_hint }]` | `.command(CommandSpec::new(name, description).arg_hint(hint))` |
| `commands: [{ name, description, request: true }]` | `.command(CommandSpec::new(name, description).request())` |
| `schedules: [{ name, every, offset }]` | `.schedule(ScheduleSpec::new(name, every).offset(offset))` |
| `triggers: ["messages"]` | `.triggers([Trigger::Messages])` |

**Handlers** are `on_message`, `on_mention`, `on_command`, `on_schedule`, `on_request` and `on_webhook`. Each receives a `Message`, `Command`, `Schedule`, `Request` or `Webhook` with the same fields as in TypeScript, in `snake_case`. They run one at a time and are not `async`.

**What `ctx` can do:**

| TypeScript | Rust |
|---|---|
| `await ctx.reply(message, text)` | `ctx.reply(&message, text)?` |
| `await ctx.post(channelId, text)` | `ctx.post(&message.channel, text)?` |
| `await ctx.react(message, emoji)` | `ctx.react(&message, emoji)?` |
| `await ctx.editOwn(message, text)` | `ctx.edit_own(&message, text)?` |
| `await ctx.deleteOwn(message)` | `ctx.delete_own(&message)?` |
| `await ctx.messages({ limit: 10 })` | `ctx.messages(MessageQuery { limit: Some(10), ..Default::default() })?` |
| `await ctx.state.getJSON<number>("n")` | `ctx.state.get_json::<u64>("n")?` (an `Option`) |
| `await ctx.state.setJSON("n", 1)` | `ctx.state.set_json("n", &1)?` |
| `await ctx.state.get("k")`, `getText`, `set`, `delete` | `ctx.state.get("k")?`, `get_text`, `set`, `delete` |
| `await ctx.fetch(url, { method, headers, body })` | `ctx.fetch(url, FetchInit::new().method("POST").header(k, v).body(bytes))?` |
| `ctx.log.info(text)` | `ctx.log.info(text)` |
| `ctx.now()` | `ctx.now()` |
| `crypto.getRandomValues(...)` | `ctx.random()` |

`FetchInit::json(&value)?` sends a JSON body. A response has `.status`, `.ok()`, `.headers.get(name)`, `.text()`, `.json::<T>()?` and `.bytes()`.

Every job starts from a fresh instance, so a `static` is reset too: keep what must last in `ctx.state`.

**Handling a refusal:**

```rust
match ctx.fetch("https://status.example.com/", FetchInit::new()) {
    Ok(response) => ctx.log.info(format!("status {}", response.status)),
    // "`http-fetch` to `status.example.com` refused: not in `egress`"
    Err(e) if e.is_refused() => ctx.log.warn(e.message()),
    Err(e) => return Err(e),
}
```

A panic also ends the job and nothing is applied, but the job's error is a generic `trap`; the panic's text appears only in the App's log. Return an error when you can.

**Testing.** `pollis app test` builds the App, then runs `cargo test`. Put tests in `tests/`:

```rust
use pollis_app::testing::{HttpFixture, JobOptions, TestApp};

#[test]
fn hello_greets_by_name() {
    let result = TestApp::new().command("hello", "Sam");
    assert!(result.ok, "{:?}", result.error);
    assert_eq!(result.replies, ["Hello, Sam!"]);
}

#[test]
fn files_a_ticket() {
    let options = JobOptions::new()
        // Answers for ctx.fetch. Tests never touch the network.
        .http(HttpFixture::new("https://api.linear.app/graphql").body(r#"{"id":"T-7"}"#))
        // State before the job.
        .state("count", "41");
    let result = TestApp::new().with(options).command("ticket", "Printer on fire");
    assert_eq!(result.fetches.len(), 1);
}
```

`TestApp` has the same calls as [`testApp()`](#testing): `send`, `message`, `mention`, `command`, `schedule`, `request`, `webhook` and `webhook_json`, and the result has the same fields (`ok`, `error`, `replies`, `posts`, `reactions`, `actions`, `state`, `logs`, `fetches`). `app.with(options)` runs one call with options: `state`, `http`, `message` (history), `grants` and `now`. `pollis app test <filter>` runs only the tests whose name contains `<filter>`.

**Building.** `pollis app build` runs `cargo build --release --target wasm32-wasip2`, reads the manifest from your code, loads the result in the real sandbox and prints the App hash. The same source, `Cargo.lock` and `rust-toolchain.toml` give the same bytes and the same App hash on any machine, in any directory. Commit all three. The first build writes `Cargo.lock`; after that a `Cargo.toml` change that needs a new lockfile stops the build until you run `cargo update --workspace`, so a dependency can never change without you seeing it.

`dev` and `run` work exactly as [above](#running-it-in-a-group-dev-build-run-approve), with `npx pollis app dev` and `npx pollis app run`.

**A complete example.** The [alerts App](../../apps/alerts/README.md) is a whole Rust App in about 30 lines, with its tests.

**Crates.** Any crate that compiles for `wasm32-wasip2` and does not need files or sockets works. A crate that opens a socket is refused at build, with the import named. Use `ctx.fetch` for HTTP.

## Troubleshooting

**`pollis-app-dev` was not found.** Build it (`cargo build --release -p pollis-app-dev` in the Pollis checkout) and put `target/release` on your `PATH`, or set `POLLIS_APP_DEV_BIN` to the binary.

**`another pollis app build is running`.** Builds take turns on one machine; run yours when the other finishes.

**A test result says `not_delivered`.** The trigger was outside what the App was granted: a channel not in `read.channels`, an edited message without `"edited"` in `read.events`, a handler whose trigger is not in `triggers`, an undeclared command (it became an ordinary message, and the App has no `onMessage`) or an undeclared schedule.

**`pollis-bot` was not found.** `dev` and `run` need the bot runtime: install it ([Self-hosting](./self-hosting.md)), put it on your `PATH`, or set `POLLIS_BOT_BIN`.

**The bot does not answer.** Check the bot's page in the group: it says whether it is waiting for an approval and why. `pollis-bot status` and the bot's log say the same on your machine.

**`the App host refuses this build`.** The component imports something the sandbox does not provide. The message names the import; this usually means a dependency reaches for the network or files directly.

**Tests fail with a TypeScript syntax error.** Your Node is older than 22.18. Check `node --version`.

**`Cargo.lock does not match Cargo.toml`.** You changed a dependency. Run `cargo update --workspace` and commit the new `Cargo.lock`.

**`the WebAssembly target is missing`.** Run `rustup target add wasm32-wasip2`. Keep the `rust-toolchain.toml` that `pollis app new` wrote: it pins the compiler your App hash depends on.
