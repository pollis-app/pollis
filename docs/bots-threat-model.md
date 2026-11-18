# Bots and Apps: threat model

Started in #2004 (the App sandbox). #2005 adds the approval model (who approves an App, what the runtime verifies before loading it, rollback protection, development installations); #2009 adds Pollis Secure Cloud. Design: epic #872.

This document states trust boundaries and the guarantees each one gives, and names the test that pins each guarantee. It describes boundaries, not open issues; those are tracked as GitHub issues.

## Terms

- **App**: code anyone can write, compiled to a WebAssembly component targeting `pollis:app@0.1.0` (`pollis-app-wit/wit/app.wit`), shipped with a manifest (`pollis-app.json`, schema `pollis-app-wit/pollis-app.schema.json`).
- **Installation**: one App approved into one group with the grants an admin chose.
- **Bot**: the installation's user. A full MLS member of exactly one group.
- **Runtime**: `pollis-bot`, the only process that holds a bot's keys or reaches the network on its behalf. It runs Apps through `pollis-app-host`.
- **Effective grants**: the manifest intersected with the admin's grants (`pollis_app_manifest::effective_grants`). The runtime enforces these and nothing wider.

## Parties and boundaries

| Party | Trusted for | Can see | Can do |
|---|---|---|---|
| Runtime (`pollis-bot` + `pollis-app-host`) | Holding the bot's keys, enforcing grants | Everything the bot decrypts | Everything the bot's grants and MLS membership allow |
| Sandbox (wasmtime, component model) | Isolating App code from the runtime | Nothing by itself | Run App code within per-job limits |
| App code | Nothing | Only what the host hands it (below) | Only what the host functions accept (below) |
| Host machine and its operator (the admin who runs the runtime) | Same as any device of the bot | What the bot decrypts | What the bot can do |
| Delivery Service (DS) | Delivery only, as for every client | Metadata, ciphertext | Withhold or delay; cannot read or forge content |
| Group admins | Choosing and narrowing an App's grants | What any member sees | Create, approve, narrow, remove bots |
| Group members | Nothing beyond membership | Bot messages like any message | Address the bot (mentions, commands, requests) |

## What the sandbox guarantees

An App runs as a WebAssembly component in `pollis-app-host`. It has no system calls and no access to host memory; it can call only the functions the host imports into it.

1. **No ambient authority.** An App may import `pollis:app/host@0.1.0` and a fixed subset of WASI 0.2, so that ordinary `std` Rust and ComponentizeJS components load. Each WASI interface is a stub that grants nothing: the environment and arguments are empty, stdin is closed, stdout and stderr go to the runtime log (never the group), `exit` ends the job with nothing applied, clocks and randomness come from the host, and the filesystem has no preopened directories, so no file can be opened. Sockets and `wasi:http` are not provided; a component that imports them, or anything else outside the subset, is refused before it runs.
   Pinned by `a_component_importing_sockets_is_refused`, `only_the_listed_wasi_interfaces_are_importable`, `an_ordinary_std_rust_app_loads_and_runs`, `the_filesystem_is_empty`, `exit_traps_the_job_and_discards_everything` (pollis-app-host `tests/enforcement.rs`, `tests/wasi.rs`).
2. **Fresh instance per job.** Each job gets a new `Store` and instance; nothing in App memory survives from one job to the next. Compiled code is cached by App hash and shared, but compiled code holds no state, and a cached entry is used only for the exact bytes it was compiled from.
   Pinned by `two_installations_of_the_same_app_share_nothing`, `different_bytes_under_a_cached_app_hash_are_refused`.
3. **Per-job limits.** A CPU deadline (epoch interruption, one-shot per job), a linear-memory cap, and caps on host calls, HTTP fetches, queries and actions. Exceeding any of them traps the job.
   Pinned by `a_timeout_discards_everything`, `a_timeout_in_one_job_does_not_stop_another`, `a_memory_overrun_discards_everything`, `exceeding_the_host_call_cap_discards_everything`, `exceeding_the_action_and_fetch_caps_traps`.
4. **All or nothing.** Actions and state writes are queued during the job and returned to the runtime only when the handler returns ok. A trap, timeout, memory overrun, cap overrun or handler error returns an error and nothing else, so nothing is applied and the installation's state is unchanged.
   Pinned by `a_trap_discards_everything`, `a_timeout_discards_everything`, `a_memory_overrun_discards_everything`, `a_handler_error_discards_everything`.

## What an App can see

- **Triggers** only of a kind its manifest declares (`triggers`), carrying a message only from a granted channel (`read.channels`) and of a granted event kind (`read.events`). The bot's own messages are never delivered to it.
  Pinned by `undeclared_trigger_kind_is_not_delivered`, `message_from_an_ungranted_channel_is_not_delivered`, `channel_the_admin_granted_but_the_manifest_did_not_request_is_not_delivered`, `undeclared_event_kind_is_not_delivered`, `the_bots_own_message_is_not_delivered`.
- **History** through `query-messages`, only from granted channels, of granted event kinds, inside the granted history window (`read.history`). The host narrows the query it sends to the runtime and filters the answer again, so a runtime bug that over-returns does not widen what the App sees.
  Pinned by `query_returns_only_granted_channels_events_and_history`.
- **Attachments** as metadata only (name, type, size).
- **State** only its own installation's: the key-value store is addressed by installation, never by App, so two installations of the same App share nothing.
  Pinned by `two_installations_of_the_same_app_share_nothing`.

## App state at rest

- An installation's App state is stored on the Delivery Service, shared by the bot's hosts, and sealed on the hosts with keys derived from the bot's account secret, which only the bot's hosts hold. Each key name becomes an HMAC under one derived key; each key and value is sealed (ChaCha20-Poly1305) under another, bound to the installation and the key, so an entry moved to another key, installation or bot does not open. The DS stores ciphertext, sizes, versions and write times.
  Pinned by `every_host_derives_the_same_keys_and_only_from_the_account_secret`, `a_sealed_value_opens_only_under_its_own_key_and_installation`, `the_key_name_never_reaches_the_wire_in_the_clear` (`pollis-core/src/commands/bots/app_state.rs`).
- State is written only by the host that won the trigger's claim, all of a job's writes or none, once per claim, and only at the versions the job read, so two hosts never overwrite each other and a retried job never writes twice. Only a bot's own hosts can read or write it; it is capped per installation and deleted with the bot.
  Pinned by `app_state_is_written_only_under_a_claim_the_writer_won_and_only_once`, `two_hosts_cannot_clobber_each_other`, `app_state_stays_under_the_ds_quota`, `app_state_endpoints_are_device_signed_for_the_signers_own_installation`, `app_state_goes_with_its_bot` (`pollis-delivery/tests/bots.rs`), `app_state_is_a_bots_versioned_and_within_the_quota`, `a_claims_state_commit_cannot_be_undone` (`bot_schema.rs`), `a_state_conflict_reruns_the_handler_on_the_new_state` (`pollis-bot/tests/app_handler.rs`) and the flows test `an_app_runs_on_asleep_hosts_that_share_its_state_beside_the_alerts_handler`.
- Nothing about an App's run is kept on a host between jobs: each job opens the installation, runs a fresh sandbox, and seals the installation again.

## What an App can do

- **In the group**: `post`, `reply`, `react`, `edit-own`, `delete-own`, each only if the action is granted (`actions`) and only in granted channels; `edit-own` and `delete-own` only on messages the bot itself sent. A refused call returns an error to the App naming the manifest field and is never applied.
  Pinned by `actions_outside_the_grants_are_refused_with_the_manifest_field`.
- **Never moderate.** There is no host function for deleting others' messages, removing members or changing roles, and no action kind for them.
  Pinned by `there_is_no_moderation_host_function`.
- **State**: sealed per-installation key-value state, within `state.quota`.
  Pinned by `state_beyond_the_quota_is_refused`, `state_without_a_quota_is_refused`.
- **Network**: `http-fetch` to hosts in the granted egress list only: HTTPS, default port, exact host match, no IP literals, no credentials in the URL, no `Host` or connection-level headers, redirects followed only within the requested host, response size capped. An App with empty egress cannot send data outside the group.
  Pinned by `http_fetch_refusals_never_reach_the_network`, `http_fetch_with_no_egress_is_refused`, `http_redirect_off_host_is_refused_and_same_host_is_followed`, `http_response_over_the_cap_is_refused`, and the unit tests in `pollis-app-host/src/egress.rs`.
- **Never into the runtime's own network.** A granted host name must resolve to a public address. The runtime's client (`PublicHttpClient`, which `HostServices::http_fetch` delegates to) resolves the name once, drops loopback, private, link-local (including cloud metadata), CGNAT, unique-local and reserved addresses (also when embedded in IPv6), refuses the request if none remain, and connects only to the addresses it checked, so a second DNS answer is never used. It follows no redirects and uses no proxy from the environment.
  Pinned by `non_public_addresses_are_refused`, `a_mixed_answer_keeps_only_public_addresses_and_looks_up_once`, `a_granted_name_that_resolves_inside_is_refused_without_connecting` (`pollis-app-host/src/http.rs`).
- **Logging** to the runtime's log, never to the group.

## Grants

- The effective grants are never wider than the manifest or the admin's grants, and admin grants wider than the manifest are refused outright rather than silently narrowed.
  Pinned by `effective_grants_never_exceed_the_manifest`, `effective_grants_never_exceed_the_admin_grants`, `widenings_name_the_manifest_field` (pollis-app-manifest) and `grants_wider_than_the_manifest_are_refused` (pollis-app-host).
- Until signed approvals land (#2005), a development build of the runtime reads admin grants from a local file. That path does not exist in release builds.
  Pinned by `dev_grants_file_is_compiled_out_of_release_builds` (pollis-app-manifest `tests/dev_grants_release.rs`).

## Stated limits

These are properties of the design, stated in developer docs and install copy:

- **An App can send what it reads to the hosts the admin approved.** Egress is the boundary; inside it, the App decides what to send. An App with no egress cannot send data outside the group.
- **The sandbox engine is in the trusted base.** Isolation rests on wasmtime and Cranelift being correct. The runtime keeps the engine's feature set minimal (no async, no GC proposals, no threads) and implements its WASI stubs itself rather than linking a general-purpose WASI implementation, to keep that base small.
- **Jobs that share a CPU can observe timing.** The sandbox isolates memory and capabilities, not microarchitectural side channels between jobs running on the same machine. Installations that must not share hardware run on separate hosts.
- **The host machine is trusted like any device of the bot.** Whoever controls the machine the runtime runs on can read what the bot can read. That is the admin's own machine for self-hosted bots; #2009 covers attested hosting.
