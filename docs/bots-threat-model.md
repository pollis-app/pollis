# Bots and Apps: threat model

Started in #2004 (the App sandbox). #2005 added the approval model ([App approval](#app-approval): who approves an App, what the runtime verifies before loading it, rollback protection, development installations); #2009 adds Pollis Secure Cloud. Design: epic #872.

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
- **Commands and mentions** by the runtime's routing, never the App's (#2018). A message reaches `on-command` only when its first word is `/<name>`, the name is declared in that installation's approved manifest (`commands`, not as a `request`), the App takes the `commands` trigger, and any bot named right after the command is this one; every other check above still applies. A `request: true` command is never routed from a channel message.
  Pinned by `routes_only_declared_non_request_commands`, `a_target_routes_to_that_bot_only` (pollis-app-manifest), `a_command_the_app_did_not_declare_or_aimed_at_another_bot_runs_nothing`, `a_command_in_a_channel_the_app_is_not_granted_runs_nothing` (`pollis-bot/tests/app_handler.rs`) and the flows tests in `src-tauri/tests/flows/bot_commands.rs`.
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
- **Webhooks** reach an App with their body, content type, user agent and `x-*` headers, never the credentials that authenticated the call (`authorization`, the signature) or cookies.
  Pinned by `an_app_installation_gets_the_raw_request_without_credentials` (`pollis-bot/src/webhook.rs`).

## Grants

- The effective grants are never wider than the manifest or the admin's grants, and admin grants wider than the manifest are refused outright rather than silently narrowed.
  Pinned by `effective_grants_never_exceed_the_manifest`, `effective_grants_never_exceed_the_admin_grants`, `widenings_name_the_manifest_field` (pollis-app-manifest) and `grants_wider_than_the_manifest_are_refused` (pollis-app-host).
- The runtime takes admin grants only from a verified approval (next section); there is no file, flag or environment variable that supplies them.
  Pinned by `without_an_approval_nothing_runs_and_admins_see_the_artifact_to_approve` (`pollis-bot/tests/app_approval.rs`).

## App approval

An installation runs exactly the App an admin approved, with exactly the grants the admin chose, and the runtime proves both to itself without trusting the DS (#2005).

**Who can approve.** A current admin of the bot's group, from one of their certified devices. The approval is a canonical (RFC 8785) record signed with that device's ML-DSA-44 key over a domain-separated message: `{v, installation_id, bot_user_id, group_id, seq, app_hash, manifest_hash, grants, approver_user_id, approver_device_id, issued_at, source, auto_update}`. The DS stores it as an opaque, append-only row keyed `(installation_id, seq)` and refuses a `seq` that is not exactly one more than the last; it holds no admin key, so it cannot forge or change one.
Pinned by `only_a_current_admin_of_the_bots_group_stores_an_approval`, `seq_moves_by_exactly_one_and_a_stale_approval_stores_nothing`, `the_schema_refuses_a_skipped_seq_an_edit_a_delete_and_a_non_bot` (`pollis-delivery/tests/bot_apps.rs`), `a_production_record_round_trips_and_verifies_under_its_own_domain_only`, `a_non_canonical_record_is_refused` (`pollis-core/src/commands/bots/app_approval.rs`).

**What the runtime verifies, before every job.** From its own state, with the same admin-signer rule as host approvals (`pollis-core/src/commands/bots/signer.rs`, shared with #1182):

1. the record decodes as the kind its row says and names this installation, bot, group and row `seq`;
2. the signature verifies under the approving device's leaf key, that device is a leaf in the runtime's own MLS view of the group, and its key is certified under the approver's account key (`IdentityDirectory`, and the transparency log does not alarm);
3. the approver is a current admin in the roster read; the MLS view and the roster must both agree;
4. the manifest served beside the record hashes to `manifest_hash`, and the grants ask for nothing the manifest did not;
5. the artifact it is about to load hashes to `app_hash` (`SHA-256("pollis-app-v1" ‖ len(component) ‖ component ‖ JCS(manifest))`, one function in `pollis-app-manifest`, with a pinned test vector). The approval binds a hash, not a location: the artifact may come from a local path or an HTTPS URL, and a mismatch is never close enough.

Any failure: the installation does not run that job, the runtime logs why with a fix, and reports it to the group's admins, who see it on the bot's page. A record that cannot be checked at all (the DS unreachable) stops the job without handling or skipping anything.
Pinned by `the_runtime_refuses_every_approval_it_cannot_verify` (flows: non-admin signer, forged signature, a device that is no leaf, a swapped manifest, widened grants, a development record served as production, another bot's record, transparency alarm, a rolled-back `seq`, a demoted and a removed approver), `an_admin_approves_narrows_and_updates_an_app_and_the_runtime_runs_exactly_that` (flows: hash mismatch, grants wider than the manifest refused before signing), `one_changed_byte_is_a_hash_mismatch_never_close_enough`, `record_checks_refuse_another_installation_a_swapped_manifest_and_widened_grants`, `app_hash_vector` (`pollis-app-manifest`), and `a_changed_artifact_stops_running_until_it_is_approved`, `a_refused_approval_runs_nothing_and_is_reported_as_refused`, `an_approval_that_cannot_be_checked_stops_the_job_without_skipping_a_message` (`pollis-bot/tests/app_approval.rs`).

**Rollback protection.** `seq` increases by one with every approval for an installation. Each host keeps the highest `seq` it accepted in the installation's encrypted store (a local trigger refuses lowering it), and refuses any record with a lower one, so the DS cannot roll an installation back to an older approved App by withholding the newer record.
Pinned by the rolled-back case in `the_runtime_refuses_every_approval_it_cannot_verify`.

**Updates and widening.** Any new App hash needs a new approval, unless the approval allows automatic updates (below). The approval page diffs the new manifest against the approved one; a widening (a new trigger kind, channel, longer history, event kind, action, egress host, larger state quota, a new slash command or one switched between posted and private request, or a new or more frequent schedule: a command or schedule is a new way for the App to be invoked) is highlighted and never pre-accepted: a non-widening update carries the current grants over and drops what the new manifest no longer asks for. An installation may be set to take non-widening updates from its pinned source automatically: `auto_update` and `source` are in the signed record (every admin sees them), and the runtime itself accepts a newer artifact from exactly that source whose manifest widens nothing against the approved one, running it with the approved grants carried to it (`check_artifact_from`). No admin has to be online. Each host records the hashes it ran under each approval in the installation's encrypted store (append-only) and refuses one it already moved past, so serving an older file cannot undo an update; the runtime reports what it runs, which admins see on the bot's page. A widening update, or any other location, waits for a new approval.
Pinned by `every_kind_of_widening_is_flagged`, `a_narrowing_update_has_no_widening`, `carried_grants_drop_what_the_new_manifest_no_longer_asks_for` (`pollis-app-manifest/src/diff.rs`), `an_installation_set_to_update_itself_takes_non_widening_updates_from_its_source_with_no_admin_online` (flows), `auto_update_takes_a_non_widening_artifact_from_the_pinned_source_only` (`app_approval.rs`), `a_non_widening_update_from_the_pinned_source_runs_with_no_admin_online`, `an_update_from_another_source_waits_for_an_admin` (`pollis-bot/tests/app_approval.rs`).

**Development installations.** A development approval is a separate record type with its own signature domain; it names a developer ML-DSA-44 key instead of an App hash. The runtime accepts any build whose App hash that key signed (domain `pollis-app-dev-build-v1`) and whose manifest does not widen the approved manifest; grants are the approved grants carried to the build's manifest. A production approval can never be read as a development one, or the reverse. Every member sees a persistent "Development build" mark on the bot.
Pinned by `a_development_approval_can_never_be_read_as_production_or_back`, `a_development_installation_runs_any_build_its_key_signs_that_does_not_widen` (`app_approval.rs`), `a_development_installation_runs_its_developers_builds_and_members_see_the_mark` (flows), `a_development_installation_runs_builds_its_developer_signed` (`pollis-bot/tests/app_approval.rs`).

**Stated limits.**

- An approval stays valid only while its approver is an admin: when the approving admin is demoted or leaves, the bot stops until another admin approves again.
- Whoever holds a development key can run any non-widening build in that installation. Development installations are for test groups.

## Stated limits

These are properties of the design, stated in developer docs and install copy:

- **An App can send what it reads to the hosts the admin approved.** Egress is the boundary; inside it, the App decides what to send. An App with no egress cannot send data outside the group.
- **Compiled Apps are cached on the host.** To keep cold starts fast, a host keeps each App's compiled code in its data directory and loads it only when the entry names the same App, the same engine build and settings, and its checksum holds; anything else is recompiled. The cache is as trusted as the rest of the runtime's data directory and is optional: a runtime without one compiles every App itself.
  Pinned by `a_cold_host_loads_what_an_earlier_one_compiled`, `a_corrupt_or_mismatched_entry_is_recompiled_never_loaded` (`pollis-app-host/tests/disk_cache.rs`).
- **The sandbox engine is in the trusted base.** Isolation rests on wasmtime and Cranelift being correct. The runtime keeps the engine's feature set minimal (no async, no GC proposals, no threads) and implements its WASI stubs itself rather than linking a general-purpose WASI implementation, to keep that base small.
- **Jobs that share a CPU can observe timing.** The sandbox isolates memory and capabilities, not microarchitectural side channels between jobs running on the same machine. Installations that must not share hardware run on separate hosts.
- **The host machine is trusted like any device of the bot.** Whoever controls the machine the runtime runs on can read what the bot can read. That is the admin's own machine for self-hosted bots; #2009 covers attested hosting.
