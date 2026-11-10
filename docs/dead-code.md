# Dead code

Four tools, because no single one sees everything. `.github/workflows/dead-code.yml` runs three of them on PRs as a gate: Knip, `dead-commands.mjs --strict` and cargo-machete each fail the PR on a finding, and print it to the job summary. The backlog they found on their first run was cleared in #1763, so a finding now is new. cargo-udeps runs on demand only and stays advisory.

| Tool | Finds | Run | In CI |
|---|---|---|---|
| Knip | Unused files, exports, types and npm dependencies | `cd frontend && pnpm dlx knip@6.44.1`, and `cd mobile && pnpm dlx knip@6.44.1 --config ../scripts/knip-mobile.jsonc` (its own pnpm project) | gate |
| `scripts/dead-commands.mjs` | Commands registered for a client that nothing in it calls | `node scripts/dead-commands.mjs` (`--strict` exits 1, `--json` for machines) | gate (`--strict`) |
| cargo-machete | Unused Rust dependencies, by text match | `cargo machete` | gate |
| cargo-udeps | Unused Rust dependencies, exactly | `cargo +nightly udeps --all-targets --no-default-features -p pollis-core ...` | `workflow_dispatch` only, advisory |

## When the gate fails

Delete what it names, or drop the `export` if the item is only used in its own file. If it really is used in a way the tool cannot see, record that below rather than switching the check off. "When in doubt, keep it" is fine, but keeping it means an exemption with the reason and, when it is waiting on work, the ticket.

## Why the command script exists

rustc counts a command listed in `generate_handler!` (or a `bridge.rs` match arm) as used, and Knip cannot follow `invoke("name")` because the call is a string. So a command nobody calls survives both. The script matches registered names against string literals in the renderer, the mobile app and `pollis-tui`. A name built at runtime is invisible to it: list it in `scripts/dead-commands.json` with the reason.

`--strict` fails on all three of its findings, not only the fully dead ones:

- **dead**: no client calls it. Remove the shim, the `generate_handler!` entry, the `bridge.rs` arm and the pollis-core implementation. Some are still exercised by an integration test, and the report says so; rewrite that test onto a live command, or delete it.
- **desktop_unused**: registered in `generate_handler!`, but only mobile or the CLI calls it. Take it out of `generate_handler!` so the shipping app does not expose it. Keep the shim if `src-tauri/src/test_harness.rs` registers it for the flows tests.
- **mobile_unused**: a `bridge.rs` arm the mobile app never names. Remove the arm.

A command one client lacks on purpose, or as tracked parity debt, goes in `scripts/dead-commands.json` with the `scripts/feature-parity.json` entry that explains it. An exemption hides the name from every category, so keep it narrow.

## False positives and where to record them

- **Knip** needs a root for anything loaded by config rather than import. The Expo plugin, the uniffi bindgen CLI and its runtime are in `scripts/knip-mobile.jsonc` `ignoreDependencies`, each with a comment. So are the transitive packages mobile deliberately loads from Expo's or expo-router's own copy (`@expo/plist`, `@expo/config-plugins`, `query-string`, ...): a direct dependency could resolve to a different version than the one the build uses. An export kept on purpose with no caller (mobile's `upper()`, the only allowed way to uppercase translated copy) carries a JSDoc `@public` tag with the reason, which Knip skips.
- **cargo-machete** matches crate names in source. A dependency declared only to switch on a feature elsewhere, or used by a file compiled in through `#[path]`, is flagged. Record it in `[package.metadata.cargo-machete] ignored` in that crate's `Cargo.toml`, with the reason above it. `src-tauri` does this for `reqwest`, and `scripts/x11-clipboard-repro` for the crates `src-tauri/src/clipboard.rs` uses.
- **cargo-udeps** runs here with `--no-default-features`, which drops the `media` feature. It then reports everything only media uses (`nnnoiseless`, `livekit-protocol`, `prost`). Those are not dead. Confirm anything it names against machete, then by deleting it and running `cargo check --all-targets`.

## Not covered

Unused `pub` items across workspace crates. rustc never flags them. `warnalyzer`, the usual answer, needs `-Z save-analysis`, which nightly removed in 2023, so it cannot run. No replacement has been vetted for this repo yet.

Unconditional `#[allow(dead_code)]` hides exactly what rustc would report, so there are none left (#1763). An item only tests use is `#[cfg(test)]`. Platform-gated ones (`cfg_attr(not(target_os = ...), allow(dead_code))`) are fine.
