# Dead code

Four tools, because no single one sees everything. All are advisory: `.github/workflows/dead-code.yml` reports to the job summary on PRs and never fails one. Drop `continue-on-error` from a step once its backlog is cleared to make it a gate.

| Tool | Finds | Run |
|---|---|---|
| Knip | Unused files, exports, types and npm dependencies | `cd frontend && pnpm dlx knip@6.44.1`, and `cd mobile && pnpm dlx knip@6.44.1 --config ../scripts/knip-mobile.jsonc` (its own pnpm project) |
| `scripts/dead-commands.mjs` | Commands registered for a client that nothing in it calls | `node scripts/dead-commands.mjs` (`--strict` exits 1, `--json` for machines) |
| cargo-machete | Unused Rust dependencies, by text match | `cargo machete` |
| cargo-udeps | Unused Rust dependencies, exactly | `cargo +nightly udeps --all-targets --no-default-features -p pollis-core ...` (workflow_dispatch only) |

## Why the command script exists

rustc counts a command listed in `generate_handler!` (or a `bridge.rs` match arm) as used, and Knip cannot follow `invoke("name")` because the call is a string. So a command nobody calls survives both. The script matches registered names against string literals in the renderer, the mobile app and `pollis-tui`. A name built at runtime is invisible to it: list it in `scripts/dead-commands.json` with the reason.

"Dead" means no client calls it. Some dead commands are still exercised by an integration test, and the report says so. Deleting one means deleting that test.

## False positives and where to record them

- **cargo-machete** matches crate names in source. A dependency declared only to switch on a feature elsewhere is flagged. Record it in `[package.metadata.cargo-machete] ignored` in that crate's `Cargo.toml`, with the reason above it. `src-tauri` does this for `reqwest`.
- **cargo-udeps** runs here with `--no-default-features`, which drops the `media` feature. It then reports everything only media uses (`nnnoiseless`, `livekit-protocol`, `prost`). Those are not dead. Confirm anything it names against machete, then by deleting it and running `cargo check --all-targets`.
- **Knip** needs a root for anything loaded by config rather than import. The Expo plugin, the uniffi bindgen CLI and its runtime are in `scripts/knip-mobile.jsonc` `ignoreDependencies`, each with a comment.

## Not covered

Unused `pub` items across workspace crates. rustc never flags them. `warnalyzer`, the usual answer, needs `-Z save-analysis`, which nightly removed in 2023, so it cannot run. No replacement has been vetted for this repo yet.

Unconditional `#[allow(dead_code)]` is worth a periodic look, since it hides exactly what rustc would report. Platform-gated ones (`cfg_attr(not(target_os = ...), allow(dead_code))`) and shared test modules are fine.
