# Pollis App reference

Everything an App can call, everything it can declare, and every way the runtime can say no. For how a job runs from trigger to applied action, read [concepts-for-developers.md](concepts-for-developers.md) first; this page does not repeat it.

The sources of truth, if this page and the code ever disagree:

- WIT interface: [`pollis-app-wit/wit/app.wit`](../../pollis-app-wit/wit/app.wit) (package `pollis:app@0.1.0`)
- Manifest schema: [`pollis-app-wit/pollis-app.schema.json`](../../pollis-app-wit/pollis-app.schema.json)
- Enforcement: `pollis-app-host` (sandbox and host functions) and `pollis-app-manifest` (manifest and grants)
- Example App: [`pollis-app-host/fixtures/test-app`](../../pollis-app-host/fixtures/test-app)

## The short version

- Your App is a WebAssembly component. It exports six handlers and can import only `pollis:app/host`. No files, no sockets, no environment variables, no WASI at all.
- Every job gets a fresh instance. Globals do not survive between jobs; use `kv-*` for anything that must.
- Nothing you do is applied until your handler returns `ok`. Return `err`, trap, run out of time or memory, and everything the job queued is thrown away.
- Your manifest says what you want. The admin can narrow it, never widen it. The runtime enforces the overlap.
- When the runtime refuses a call, you get an error string that names the manifest field to change, for example ``"`http-fetch` to `api.y.com` refused: not in `egress`"``.

## Handlers (exports)

Every App exports all six. The runtime calls only the ones whose trigger is in your manifest's `triggers` (and the admin's grants allow); an App that only handles commands can return `ok` from the others. Each returns `result<_, string>`: `ok` applies what the job queued, `err(message)` discards it and logs `message` on the runtime.

| Export | Called when | Manifest trigger | Gets |
|---|---|---|---|
| `on-message` | A message arrives in a granted channel | `messages` | `message` |
| `on-mention` | A message mentions the bot | `mentions` | `message` |
| `on-command` | A slash command is addressed to the App | `commands` | `command { name, args, message }` |
| `on-schedule` | A schedule fires | `schedule` | `schedule { name, slot }` (`slot` = unix ms) |
| `on-request` | A member sends the App a request | `requests` | `request { id, requester, body }` |
| `on-webhook` | An HTTP call reaches the App's webhook | `webhook` | `webhook { delivery-id, headers, body }` |

A message-carrying trigger (message, mention, command) is delivered only if **all** of these hold, otherwise your App is not run at all:

- its channel is in your effective `read.channels`;
- its kind is in `read.events`: a message with `edited-at` set is an `edited` event, otherwise `created`;
- it was not sent by your own bot (so an App can never loop on its own posts).

### The data you receive

```
message {
  id, channel,                     // opaque ids
  author { id, display-name, is-bot },
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

Queue a new message in `channel` (a channel id). Applied only if the handler returns `ok`.

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

Until the runtime keeps a message log (#2008), this returns the current trigger's context only.

| Refusal | When |
|---|---|
| ``"`query-messages` on `<channel>` refused: not in `read.channels`"`` | asked for an ungranted channel |

### `kv-get(key)` · `kv-set(key, value)` · `kv-delete(key)`

Sealed key-value state for **this installation**. Two groups running the same App never see each other's state. Writes become durable only if the handler returns `ok`; within a job, `kv-get` sees the job's own writes.

Usage is the sum of `key length + value length` over all keys, and must stay within `state.quota`.

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
| ``"`http-fetch` to `<host>` failed: <reason>"`` | network error |

### `log(level, text)`

Writes to the runtime's log (levels `trace`, `debug`, `info`, `warn`, `error`). Never reaches the group. Text over 4 KiB is truncated.

### `now() -> u64` · `random() -> u64`

Unix milliseconds, and a cryptographically random 64-bit value.

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
  "egress": ["api.linear.app"]
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

Ask for the least you need. Admins see every field in plain language before approving, and a later version that asks for more needs fresh approval.

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

During local development, before signed approvals exist (#2005), a debug build of the runtime reads the admin's grants from a JSON file named by `POLLIS_APP_DEV_GRANTS`:

```json
{
  "channels": [{ "id": "<channel id>", "name": "support" }],
  "history": 7,
  "events": ["created"],
  "actions": ["reply", "react"],
  "egress": ["api.linear.app"]
}
```

Release builds of the runtime cannot read this file at all.

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

## Your first App (Rust)

The repo's fixture App is a complete, working example: [`pollis-app-host/fixtures/test-app/src/lib.rs`](../../pollis-app-host/fixtures/test-app/src/lib.rs). Here is the smallest useful App in the same style: it replies "pong" to any message that says "ping".

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
wit-bindgen = { version = "0.61", default-features = false, features = ["macros"] }

[profile.release]
opt-level = "s"
lto = true
strip = true
panic = "abort"
```

**3. Write it** (`src/lib.rs`). The handler is at the top; everything below the `no_std plumbing` line is boilerplate every `no_std` App copies unchanged.

Why `no_std`: Rust's standard library on `wasm32-wasip2` always imports WASI interfaces (stdio, environment, exit), even from a crate that never prints, and the runtime refuses any component that imports anything but `pollis:app/host`. Without `std` you need an allocator, a panic handler, and two small functions the standard library would otherwise supply.

```rust
// No standard library: a std build on wasm32-wasip2 imports WASI
// (stdio, environment, exit), and the runtime refuses those.
#![no_std]

extern crate alloc;

use alloc::string::String;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::Cell;

wit_bindgen::generate!({ path: "wit", world: "app" });

use pollis::app::host;

struct PingApp;

impl Guest for PingApp {
    fn on_message(message: Message) -> Result<(), String> {
        if message.text.trim() == "ping" {
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

// --- no_std plumbing: copy as is ---------------------------------------

// A bump allocator. Never frees, which is fine: the instance lives for one job.
struct Bump(Cell<usize>, Cell<usize>);
unsafe impl Sync for Bump {}

unsafe impl GlobalAlloc for Bump {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let start = (self.0.get() + layout.align() - 1) & !(layout.align() - 1);
        let end = start + layout.size();
        if self.0.get() == 0 || end > self.1.get() {
            let pages = layout.size().div_ceil(65536) + 1;
            let prev = core::arch::wasm32::memory_grow(0, pages);
            if prev == usize::MAX {
                return core::ptr::null_mut();
            }
            let base = (prev * 65536 + layout.align() - 1) & !(layout.align() - 1);
            self.0.set(base + layout.size());
            self.1.set((prev + pages) * 65536);
            return base as *mut u8;
        }
        self.0.set(end);
        start as *mut u8
    }
    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
}

#[global_allocator]
static ALLOC: Bump = Bump(Cell::new(0), Cell::new(0));

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

// The canonical ABI's allocator entry point.
#[no_mangle]
unsafe extern "C" fn cabi_realloc(old: *mut u8, old_len: usize, align: usize, new_len: usize) -> *mut u8 {
    if new_len == 0 {
        return align as *mut u8;
    }
    let new = ALLOC.alloc(Layout::from_size_align_unchecked(new_len, align));
    if !old.is_null() {
        core::ptr::copy_nonoverlapping(old, new, old_len.min(new_len));
    }
    new
}

// No libc is linked, so provide the one routine the compiler still calls.
#[no_mangle]
unsafe extern "C" fn memcmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    for i in 0..n {
        let (x, y) = (*a.add(i), *b.add(i));
        if x != y {
            return x as i32 - y as i32;
        }
    }
    0
}
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

**If the runtime refuses your component**, check its imports. Anything other than `pollis:app/host` is refused before your code runs:

```
import `wasi:cli/environment@0.2.6` refused: an App may import only `pollis:app/host@0.1.0` (no WASI filesystem, sockets or environment)
```

The usual cause is a dependency that needs `std`. Use `host::log` instead of printing, and pick `no_std` crates (most of `alloc` works: `String`, `Vec`, `format!`, `BTreeMap`). The fixture App in the repo is a larger `no_std` example.

**6. Run it.** The TypeScript SDK and the `pollis app` CLI (`new`, `test`, `dev`, `build`, `run`) are #2006. Until then, the host's own tests show how a job is driven end to end: [`pollis-app-host/tests/enforcement.rs`](../../pollis-app-host/tests/enforcement.rs) runs the fixture through every trigger and host function, and `pollis-app-host/fixtures/build.sh` rebuilds it.
