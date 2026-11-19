# Pollis App reference

Everything an App can call, everything it can declare, and every way the runtime can say no. For how a job runs from trigger to applied action, read [concepts-for-developers.md](concepts-for-developers.md) first; this page does not repeat it.

Writing in TypeScript? Start with [the developer guide](developer-guide.md). `@pollis-inc/app` wraps every function below as a promise on `ctx` (`reply` is `ctx.reply`, `http-fetch` is `ctx.fetch`, `kv-get` is `ctx.state.get`, and so on), and its refusals are the same strings. Writing in Rust? The `pollis-app` crate wraps them the same way ([Writing your App in Rust](developer-guide.md#writing-your-app-in-rust)): `reply` is `ctx.reply`, `http-fetch` is `ctx.fetch`, `kv-get` is `ctx.state.get`, and a refusal is an error whose `is_refused()` is true.

The sources of truth, if this page and the code ever disagree:

- WIT interface: [`pollis-app-wit/wit/app.wit`](../../pollis-app-wit/wit/app.wit) (package `pollis:app@0.1.0`)
- Manifest schema: [`pollis-app-wit/pollis-app.schema.json`](../../pollis-app-wit/pollis-app.schema.json)
- Enforcement: `pollis-app-host` (sandbox and host functions) and `pollis-app-manifest` (manifest and grants)
- Example App: [`pollis-app-host/fixtures/test-app`](../../pollis-app-host/fixtures/test-app)

## The short version

- Your App is a WebAssembly component. It exports six handlers and reaches the world only through `pollis:app/host`. Ordinary `std` Rust (and ComponentizeJS) works: the runtime provides the WASI basics as harmless stubs. There are no files, no sockets and no environment variables (see [WASI](#wasi-what-std-gets)).
- Every job gets a fresh instance. Globals do not survive between jobs; use `kv-*` for anything that must.
- Nothing you do is applied until your handler returns `ok`. Return `err`, trap, run out of time or memory, and everything the job queued is thrown away.
- Your manifest says what you want. The admin can narrow it, never widen it. The runtime enforces the overlap.
- When the runtime refuses a call, you get an error string that names the manifest field to change, for example ``"`http-fetch` to `api.y.com` refused: not in `egress`"``.

## Handlers (exports)

Every App exports all six. The runtime calls only the ones whose trigger is in your manifest's `triggers` (and the admin's grants allow); an App that only handles commands can return `ok` from the others. Each returns `result<_, string>`: `ok` applies what the job queued, `err(message)` discards it and logs `message` on the runtime.

| Export | Called when | Manifest trigger | Gets |
|---|---|---|---|
| `on-message` | A message arrives in a granted channel | `messages` | `message` |
| `on-mention` | A message names the bot by `@username` | `mentions` | `message` |
| `on-command` | A member sends one of the App's declared [commands](#commands) | `commands` | `command { name, args, message }` |
| `on-schedule` | One of your [schedules](#schedules) reaches a slot | `schedule` | `schedule { name, slot }` (`slot` = unix ms) |
| `on-request` | A member sends the App a request (arrives with the message log, #2008) | `requests` | `request { id, requester, body }` |
| `on-webhook` | An HTTP call reaches the App's webhook | `webhook` | `webhook { delivery-id, headers, body }`: the request body as sent, its `content-type` and `user-agent`, and its `x-*` headers (never `authorization`, cookies or the signature) |

A message is delivered once: a declared [command](#commands) goes to `on-command`; otherwise, if it mentions the bot and your App takes `mentions`, to `on-mention`; otherwise, if your App takes `messages`, to `on-message`. A mention is `@` plus the bot's username at the start of the message or after a space, any case, trailing punctuation ignored (the same rule that decides whom a channel message notifies). `@all` is not a mention of the bot.

A message-carrying trigger (message, mention, command) is delivered only if **all** of these hold, otherwise your App is not run at all:

- its channel is in your effective `read.channels`;
- its kind is in `read.events`: a message with `edited-at` set is an `edited` event, otherwise `created`;
- it was not sent by your own bot (so an App can never loop on its own posts).

### The data you receive

```
message {
  id, channel,                     // opaque ids
  author { id, display-name, is-bot },   // display-name: the member's username; is-bot: another bot
  text,
  attachments: list<attachment { name, mime-type, size }>,   // metadata only, no bytes
  reply-to: option<message-id>,
  thread: option<message-id>,
  sent-at: u64,                    // unix milliseconds
  edited-at: option<u64>,
}
```

Attachment bytes are not available to Apps yet; you get name, MIME type and size.

## Host functions (imports)

Every call counts toward the per-job host-call cap (see [Limits](#per-job-limits)). Calls that can be refused return `result`; a refusal is an ordinary `err` you can handle, it does not stop your job. Hitting a cap is different: it traps and ends the job.

### `post(channel, text) -> result<_, string>`

Queue a new message in `channel`: a granted channel's id, or its name (with or without `#`, any case). A name two granted channels share is refused; use the id. Applied only if the handler returns `ok`.

| Refusal | When |
|---|---|
| ``"`post` refused: not in `actions`"`` | `post` is not granted |
| ``"`post` to `<channel>` refused: not in `read.channels`"`` | the channel is not granted |
| ``"`post` refused: text exceeds 16384 bytes"`` | text too long |

### `reply(message, text)` · `react(message, emoji)`

Queue a reply to, or a reaction on, an existing message. The message must be one in a granted channel: the trigger's own message, one returned by `query-messages`, or one the runtime can find in a granted channel.

| Refusal | When |
|---|---|
| ``"`reply` refused: not in `actions`"`` (or `react`) | action not granted |
| ``"`reply` on `<id>` refused: not a message in `read.channels`"`` | unknown message, or in a channel you were not granted |
| ``"`reply` refused: text exceeds 16384 bytes"`` | text (or emoji) too long |

### `edit-own(message, text)` · `delete-own(message)`

Queue an edit or deletion of a message **your bot sent**. Same rules as `reply`, plus:

| Refusal | When |
|---|---|
| ``"`edit-own` on `<id>` refused: not a message this bot sent"`` | someone else's message |
| ``"`delete-own` on `<id>` refused: not a message this bot sent"`` | someone else's message |

There is no way to delete other people's messages, remove members or change roles. Bots never moderate.

### `query-messages(filter) -> result<list<message>, string>`

```
message-filter { channel: option<channel-id>, since: option<u64>, until: option<u64>, limit: u32 }
```

Returns messages from granted channels only (all of them when `channel` is absent), of granted event kinds, sent no earlier than `now - read.history days` (an earlier `since` is clamped). The trigger's own message is always returnable. At most `limit`, and never more than 500.

Until the runtime keeps a message log (#2008), this returns the trigger's own message only.

| Refusal | When |
|---|---|
| ``"`query-messages` on `<channel>` refused: not in `read.channels`"`` | asked for an ungranted channel |

### `kv-get(key)` · `kv-set(key, value)` · `kv-delete(key)`

Key-value state for **this installation**, shared by every host of the bot and kept between jobs. Two groups running the same App never see each other's state.

- It is stored on the Delivery Service, sealed with keys only the bot's hosts hold. The Delivery Service sees ciphertext and sizes, never your key names or values.
- Within a job, `kv-get` sees the job's own writes. Writes are saved only if the handler returns `ok` and this host wins the trigger's claim.
- If another host changed a key you read since you read it, your writes are not saved and the runtime runs your handler again on the new state (up to three times). Write handlers so that running them again is fine: read, compute, write.

Usage is the sum of `key length + value length` over all keys, and must stay within `state.quota`. Independently, an installation can never hold more than 1 MiB of sealed state.

| Refusal | When |
|---|---|
| ``"`kv-get` refused: no `state.quota` declared"`` (any kv call) | `state.quota` is 0 |
| ``"`kv-set` refused: keys are 1 to 256 bytes"`` | empty or long key |
| ``"`kv-set` refused: would exceed `state.quota` (<n> bytes)"`` | over quota |

### `http-fetch(request) -> result<http-response, string>`

```
http-request  { method, url, headers: list<tuple<string,string>>, body: option<list<u8>> }
http-response { status: u16, headers, body: list<u8> }
```

One HTTPS request to a host in your effective `egress`. Rules:

- `https://` only, default port only, exact host match (`api.x.com` does not allow `x.com` or `eu.api.x.com`).
- No IP addresses in any spelling (`127.0.0.1`, `2130706433`, `[::1]`).
- The host name must resolve to a public address. Names that resolve only to loopback, private, link-local (including cloud metadata such as `169.254.169.254`), CGNAT, unique-local or reserved addresses are refused, and the runtime connects to the address it checked, so a changed DNS answer cannot redirect the request inward.
- No `user:pass@` in the URL.
- You may not set `Host`, `Connection`, `Transfer-Encoding`, `Upgrade`, `TE`, `Trailer`, `Keep-Alive` or `Proxy-*` headers.
- Methods: GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS.
- Redirects are followed (up to 3) only when they stay on the same host. A 303 becomes a GET.
- Response bodies over 1 MiB are refused.

| Refusal | When |
|---|---|
| ``"`http-fetch` to `<host>` refused: not in `egress`"`` | host not granted (or egress is empty) |
| ``"`http-fetch` to `<host>` refused: only https is allowed"`` | `http://` |
| ``"`http-fetch` to `<host>` refused: only the default https port is allowed"`` | `:8443` and friends |
| ``"`http-fetch` to `<ip>` refused: IP literals are not allowed; list a host name in `egress`"`` | IP address |
| ``"`http-fetch` to `<host>` refused: credentials in the URL are not allowed"`` | `user@host` |
| ``"`http-fetch` refused: header `<name>` may not be set by an App"`` | forbidden header |
| ``"`http-fetch` refused: method "<m>" is not allowed"`` | e.g. `CONNECT` |
| ``"`http-fetch` redirect from `<a>` to `<b>` refused: redirects may not leave the requested host"`` | off-host redirect |
| ``"`http-fetch` to `<host>` refused: more than 3 redirects"`` | redirect loop |
| ``"`http-fetch` to `<host>` refused: response exceeds 1048576 bytes"`` | body too large |
| ``"`http-fetch` to `<host>` failed: ... `<host>` resolves only to non-public addresses (loopback, private, link-local or metadata)"`` | the name points inside a private network |
| ``"`http-fetch` to `<host>` failed: <reason>"`` | network error or timeout |

### `log(level, text)`

Writes to the runtime's log (levels `trace`, `debug`, `info`, `warn`, `error`). Never reaches the group. Text over 4 KiB is truncated.

### `now() -> u64` · `random() -> u64`

Unix milliseconds, and a cryptographically random 64-bit value.

## WASI: what `std` gets

So that ordinary `std` Rust and ComponentizeJS components load, the runtime provides these WASI 0.2 interfaces (any 0.2.x version). None of them reaches anything outside the sandbox:

| Interface | What your App sees |
|---|---|
| `wasi:cli/environment` | No environment variables, no arguments. `std::env::current_dir()` reports `/`, which contains nothing. |
| `wasi:cli/stdout`, `stderr` | `println!` / `eprintln!` / `console.log` go to the runtime log at debug level, one entry per line, 64 KiB per job. Never to the group. |
| `wasi:cli/stdin` | Always at end of input. |
| `wasi:cli/exit` | `std::process::exit` ends the job like a trap: nothing is applied. Return from the handler instead. |
| `wasi:clocks/wall-clock` | The runtime's time (same as `now()`). |
| `wasi:clocks/monotonic-clock` | Works, including sleeps and timers; a sleep past the job deadline ends the job. |
| `wasi:random/*` | Cryptographically random bytes. |
| `wasi:filesystem/*` | No directories are opened for you, so every file operation fails (`std::fs::read` returns an error). |
| `wasi:io/*` | Streams and pollables backing the above. |

Anything else is refused when the App is loaded, before any of your code runs:

```
import `wasi:sockets/tcp@0.2.6` refused: Apps have no sockets or direct network access; use `http-fetch` with hosts listed in `egress`
```

That covers `std::net`, `wasi:sockets` and `wasi:http`. Reach the network through `http-fetch`.

## The manifest: `pollis-app.json`

Shipped beside the component. Unknown fields are an error.

```json
{
  "$schema": "https://pollis.com/schemas/pollis-app/0.1.0/pollis-app.schema.json",
  "name": "linear-triage",
  "version": "1.2.0",
  "description": "Files /bug reports from #support as Linear issues.",
  "triggers": ["commands", "mentions"],
  "read": {
    "channels": ["#support"],
    "history": 7,
    "events": ["created"]
  },
  "actions": ["reply", "react"],
  "state": { "quota": 65536 },
  "egress": ["api.linear.app"],
  "commands": [
    { "name": "bug", "description": "File a Linear issue", "arg_hint": "<title>" }
  ]
}
```

| Field | Type | Default | Meaning |
|---|---|---|---|
| `$schema` | string | none | Editor hint; ignored. |
| `name` | string | required | Lowercase letters, digits and `-`, 1 to 64 characters, not starting with `-`. |
| `version` | string | required | Your version, shown to admins. Not empty. |
| `description` | string | `""` | Shown to admins. |
| `triggers` | list | `[]` | Any of `messages`, `mentions`, `commands`, `schedule`, `requests`, `webhook`. |
| `read.channels` | list | `[]` | `"#name"` for a channel by name, or `"*"` for whichever channels the admin grants. Empty means no channel: no message triggers, no queries, no posts. |
| `read.history` | integer (days) | `0` | How far back `query-messages` may reach. |
| `read.events` | list | `["created"]` | `created` (new messages), `edited` (edited messages). |
| `actions` | list | `[]` | Any of `post`, `reply`, `react`, `edit_own`, `delete_own`, `transfer_history` (the last is reserved for #2008). |
| `state.quota` | integer (bytes) | `0` | Key-value state allowance. 0 means no state. |
| `egress` | list | `[]` | Exact lowercase host names. No `*`, scheme, port, path or IP. Empty means your App cannot send data outside the group, and admins see a mark saying so. |
| `commands` | list | none | Required with the `commands` trigger and refused without it. Slash commands members can pick in the composer; see [Commands](#commands). |
| `schedules` | list | none | Required with the `schedule` trigger and refused without it. Each `{ "name", "every", "offset" }`; see [Schedules](#schedules). |

### Schedules

```json
"triggers": ["schedule"],
"schedules": [
  { "name": "daily-digest", "every": 86400, "offset": 32400 },
  { "name": "poll-status", "every": 300 }
]
```

- `name`: lowercase letters, digits and `-`, 1 to 32 characters; passed to `on-schedule`.
- `every`: the period in seconds, 60 (one minute) to 604800 (one week).
- `offset`: seconds after each period boundary, less than `every` (default 0). Slots fall at every Unix time `k × every + offset`, in UTC: `daily-digest` above runs at 09:00 UTC.

Every host of the bot computes the same slots, and each slot runs once across them. The runtime sets one timer to the next slot; there is no polling. A host that was asleep at a slot runs it when it next wakes (a message, a webhook call, its next start or `run --once`), up to the **three most recent** missed slots of each schedule; older missed slots are skipped and logged. A schedule seen for the first time starts at the current slot: an App never receives slots from before it was installed.

Ask for the least you need. Admins see every field in plain language before approving, and a later version that asks for more needs fresh approval.

## Commands

A command is how a member asks your App to do something: they type `/`, pick your command from the list, add any arguments and send. Declare each one in the manifest:

```json
"triggers": ["commands"],
"commands": [
  { "name": "bug", "description": "File a Linear issue", "arg_hint": "<title>" },
  { "name": "standup", "description": "Post today's standup thread" }
]
```

| Field | Required | Rules |
|---|---|---|
| `name` | yes | What members type after `/`. Lowercase letters, digits, `-` and `_`, starting with a letter, up to 32 characters. Unique within your manifest. |
| `description` | yes | One line, up to 100 characters, shown beside the name in the composer. |
| `arg_hint` | no | What to type after the name, shown greyed (`<title>`, `[days]`). Leave it out if the command takes no arguments. |
| `request` | no, default `false` | Reserved for member requests (#2008): a `request` command is delivered privately to your bot's `on-request` and never posted in the channel. It needs the `requests` trigger instead of `commands`. |

A manifest may declare up to 25 commands. Like `schedules`, `commands` and its trigger go together: a command needs `commands` in `triggers` (a `request` one needs `requests`), and the `commands` trigger needs at least one command that is not a `request`. The manifest is refused otherwise, with an error naming what is missing.

### What arrives in `on-command`

A member sends `/bug @linear_bot Printer is on fire`. Your handler gets:

```
command {
  name: "bug",                    // as declared, lowercase
  args: "Printer is on fire",     // everything after the name (and the bot's @handle), trimmed
  message: message { ... },       // the message itself, for reply(), react() and its channel
}
```

`args` is the raw text: split or parse it yourself. Reply with `reply(command.message.id, ...)` (needs the `reply` action) or `post(command.message.channel, ...)` (needs `post`).

### How the runtime routes a command

On the wire a command is an **ordinary message** whose text starts with `/<name>`. Members on any version of Pollis see it as a normal message, and a command sent while your bot is offline is handled when it catches up. The runtime, never your App, decides where each message goes; every message reaches exactly one of your handlers:

1. `on-command`, if the message's first word is `/<name>`, `<name>` is declared in your **approved** manifest (not as a `request`), your App takes the `commands` trigger, and the next word is either not an `@mention` or is your bot's `@handle`;
2. otherwise `on-mention`, if the message mentions your bot and your App takes `mentions`;
3. otherwise `on-message`, if your App takes `messages`.

The channel and event checks [above](#handlers-exports) apply to all three: a command in a channel your App is not granted reaches no handler, and the composer does not offer it there.

**Two Apps with the same command name.** Names are yours to choose, so two Apps in one group may both declare `/ticket`. The composer then shows each with its App's name (`/ticket · linear`) and, when the member picks one, inserts that bot's `@handle` after the command: `/ticket @linear_bot ...`. A command that names a bot reaches only that bot, so the message reaches exactly the App the member picked. A command typed without a bot reaches every App in the group that declares it, which is why the composer does not send a shared name without one.

## Effective grants

The runtime enforces **effective grants = your manifest ∩ the admin's grants**:

| Dimension | Effective value |
|---|---|
| Triggers | Your `triggers` |
| Channels | The channels the admin granted whose names your `read.channels` selects (`"*"` selects all of them) |
| History | The shorter of your `read.history` and the admin's |
| Events, actions, egress | The intersection of your list and the admin's |
| State quota | Your `state.quota` |

An admin grant that is wider than your manifest (a channel, host or action you did not ask for) is refused outright, so the App does not run until the grant is fixed.

The admin's grants come from their signed approval and nothing else: an admin chooses them on the bot's page when approving your App ([Approving an App](./app-approval.md)), and can narrow them later. The runtime checks the approval before every job.

## Per-job limits

| Limit | Default | When exceeded |
|---|---|---|
| Wall-clock deadline | 10 s | job ends (`Timeout`) |
| Linear memory | 64 MiB | job ends (`MemoryExceeded`) |
| Host calls (all functions, including `log`, `now`, `random`) | 10,000 | job ends (`HostCallLimit`) |
| `http-fetch` requests (each redirect hop counts) | 16 | job ends |
| `query-messages` calls | 16 | job ends |
| Queued actions | 32 | job ends |
| Messages per query | 500 | result truncated |
| HTTP response body | 1 MiB | that call refused |
| Text per action | 16 KiB | that call refused |
| kv key | 256 bytes | that call refused |
| Log line | 4 KiB | truncated |

"Job ends" means the same as an `err` return: nothing queued is applied and no state is written.

## Your first App (Rust, without the SDK)

Most Rust Apps should use the `pollis-app` crate and `pollis app new --lang rust`, which handle everything below for you ([Writing your App in Rust](developer-guide.md#writing-your-app-in-rust)). This section builds the same kind of component by hand from the WIT, which is useful if you want no dependencies beyond `wit-bindgen`.

The smallest useful App: it replies "pong" to any message that says "ping". Ordinary `std` Rust, nothing special.

**1. Install the target** (once):

```bash
rustup target add wasm32-wasip2
```

**2. Create the crate:**

```bash
cargo new --lib ping-app && cd ping-app
mkdir wit && cp <pollis repo>/pollis-app-wit/wit/app.wit wit/
```

`Cargo.toml`:

```toml
[package]
name = "ping-app"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.61"

[profile.release]
opt-level = "s"
lto = true
strip = true
```

**3. Write it** (`src/lib.rs`):

```rust
wit_bindgen::generate!({ path: "wit", world: "app" });

use pollis::app::host;

struct PingApp;

impl Guest for PingApp {
    fn on_message(message: Message) -> Result<(), String> {
        if message.text.trim() == "ping" {
            // Goes to the runtime's log at debug level, never to the group.
            println!("ping from {}", message.author.display_name);
            // A refusal comes back as Err with the reason; pass it on.
            host::reply(&message.id, "pong")?;
        }
        Ok(())
    }

    // Not declared in the manifest, so never called.
    fn on_mention(_: Message) -> Result<(), String> { Ok(()) }
    fn on_command(_: Command) -> Result<(), String> { Ok(()) }
    fn on_schedule(_: Schedule) -> Result<(), String> { Ok(()) }
    fn on_request(_: Request) -> Result<(), String> { Ok(()) }
    fn on_webhook(_: Webhook) -> Result<(), String> { Ok(()) }
}

export!(PingApp);
```

**4. Declare what it needs** (`pollis-app.json`):

```json
{
  "name": "ping",
  "version": "0.1.0",
  "description": "Answers ping with pong.",
  "triggers": ["messages"],
  "read": { "channels": ["*"] },
  "actions": ["reply"]
}
```

No `egress`, no `state`: this App cannot send anything outside the group and keeps no data.

**5. Build:**

```bash
cargo build --release --target wasm32-wasip2
# -> target/wasm32-wasip2/release/ping_app.wasm
```

If the runtime refuses the component, the message names the import that caused it (usually a dependency using `std::net`; see [WASI](#wasi-what-std-gets)). The repo has two complete examples: [`fixtures/std-app`](../../pollis-app-host/fixtures/std-app/src/lib.rs) (ordinary `std`) and [`fixtures/test-app`](../../pollis-app-host/fixtures/test-app/src/lib.rs) (`no_std`, for the smallest possible component).

**6. Run it.**

1. Pair the bot as usual (`pollis-bot pair`, see [Self-hosting](./self-hosting.md)).
2. Put `pollis-app.json` and your `.wasm` in one directory (or publish both at an `https://` address) and point the installation at it:

   ```bash
   pollis-bot app install --installation <bot id> --artifact ./dist
   # or: --artifact https://example.com/my-app/v1/my_app.wasm   (pollis-app.json beside it)
   ```

   This checks that the manifest is valid and the component compiles and imports only what the sandbox provides, and prints the **App hash**. Nothing is copied: the runtime reads the artifact where it is before every job.
3. Start (or restart) the runtime: `pollis-bot run`. The bot's page in Pollis now shows **Waiting for your approval**; a group admin checks the App hash and approves ([Approving an App](./app-approval.md)). The App runs from the next message on.

**While you develop**, approving every build would be slow. Make a developer key once and sign each build with it; an admin approves the key once, as a **development installation**, in a test group:

```bash
pollis-bot app dev-key                          # creates <data dir>/developer-key, prints its public half
pollis-bot app sign --artifact ./dist           # after every build: writes my_app.wasm.sig.json
```

After that, every build the key signs runs, as long as its manifest asks for no more than the approved one. Members of that group see a **Development build** mark on the bot.

The first time a runtime loads an App it compiles it (well under a second for a typical Rust App; about 0.6 s for a 12 MB TypeScript App on a many-core machine) and keeps the result in `<data dir>/app-cache/`, so later starts, including `run --once`, load it in milliseconds. Deleting that directory is always safe.

The repo's tests show the same path end to end: [`pollis-bot/tests/app_handler.rs`](../../pollis-bot/tests/app_handler.rs) runs the fixture App through the job loop, and the flows tests in `src-tauri/tests/flows/bot_runtime.rs` run it on real hosts of one bot.
