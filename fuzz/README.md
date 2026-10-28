# `pollis-fuzz` — Track-B cargo-fuzz targets (#481)

Coverage-guided (`libFuzzer`) fuzz targets over the load-bearing **pure
functions** of the MLS control-plane / delivery state machine — the same
functions Kani proves in `pollis-core`
(`docs/machine-checked-correctness-design.md` §4, §5.1 Track B). Each target
fuzzes the **real production function** (no forked copy) and asserts the **same
property** its Kani harness proves, so this is continuous, coverage-guided
exploration *between* Kani's bounded proof and the system-level marathon soak.
Because the pure fns have no un-seedable RNG, these targets are **OSS-Fuzz
eligible** (each has a seed corpus under `corpus/<target>/`).

| Target | Fn (crate path) | Invariant | Property asserted |
|---|---|---|---|
| `next_watermark` | `commands::messages::watermark::next_watermark` | I3 | P1 no-skip (watermark strictly below the first un-handled `sent_at`), P2 monotone, P3 handled-liveness |
| `classify` | `commands::mls::invariants::classify` | I1 | never `Apply` across a gap (`Apply` ⟺ next row epoch == current) |
| `resolve` | `commands::mls::invariants::resolve` | I2 | `Adopt` ⟺ stored bytes == ours; `Rollback` ⟹ stored != ours |
| `may_rejoin` | `commands::mls::invariants::may_rejoin` | I5 | `result ⟺ (registered && is_member)` |

Since #1537 six more targets go past pure decision functions. They reach
pollis-core through the `fuzzing` feature (`commands::{messages,mls}::fuzz_surface`),
which wraps the production functions without copying them:

| Target | Drives | Property asserted | Mutant |
|---|---|---|---|
| `mls_delivery` | real openmls groups on pollis-core's provider, seal (`try_mls_encrypt`), `MlsDecryptor` and `into_sealing_order`, over a model DS with the epoch gate. Fuzzed: sends in 2-4 channels, self-updates, device adds and removes, catch-ups after any absence, kills after a decrypt or after a seal | zero loss (#1524): every message that landed at epoch `e` is stored on every current member that joined by `e`, on none that joined later, and no outbox is left holding one | decrypt and row not one transaction |
| `decrypt_untrusted` | `MlsDecryptor::decrypt`, `envelope_lineage` on mutated genuine envelopes and raw bytes | no panic; anything that opens is a genuine envelope's exact plaintext | "decrypts" by header |
| `framing` | `messages::framing` (`classify`, `strip`, `thread_of`, every encoder) | no panic; a buffer read as a redaction or receipt is exactly that value's encoding; every encoder round-trips | lenient redaction parse |
| `ds_bodies` | the message-path `pollis-api` bodies, `pollis_delivery::messages::check_cursor_stamp` | no panic; an accepted body round-trips unchanged; admitted stamps sort by instant | lenient stamp check |
| `sealing_order` | `ingest::partition_envelopes` + `into_sealing_order`, on a modelled DS numbering and on raw `CatchUpResponse` JSON | each conversation gets exactly its envelopes in `seq` order; the run is the senders' order; no arrival (older DS) changes nothing | conversation-by-conversation order |
| `outbox` | the outbox's row transitions on a real local schema | never duplicated, never dropped from the drain, never demoted | unguarded failure mark |

The renderer half of the outbox (`pendingMessages.ts`) is TypeScript and out
of reach here; `frontend/tests/` and `mobile/tests/pending-messages.test.ts` pin it.

## Toolchain — nightly, OUT OF BAND

cargo-fuzz needs **nightly**; this repo pins **Rust 1.96.0 stable** via
`rust-toolchain.toml` for reproducible/verifiable release builds. This crate is
therefore **detached from the workspace** — it carries its own `[workspace]`
table *and* is listed in the root `Cargo.toml`'s `workspace.exclude`, so a plain
`cargo build` / the pinned-stable release path never sees it. Fuzzing runs
**out of band on nightly, never as part of the release**.

```bash
rustup toolchain install nightly
cargo install cargo-fuzz

# build + short-run all four targets (the smoke gate)
./scripts/fuzz-check.sh

# run one target longer
cargo +nightly fuzz run next_watermark corpus/next_watermark
```

pollis-core is pulled in with `default-features = false` so the fuzz crate does
not drag in the Tauri/native/media stack.

## Negative check (teeth)

Each target has a deliberate **mutant** behind `--cfg fuzz_mutant`: with the cfg
on, the target calls a locally-reimplemented **buggy** variant of the function
(e.g. `next_watermark` breaks on `>` instead of `>=`, `may_rejoin` uses `||`
instead of `&&`) and the fuzzer trips the property fast. The committed default is
**clean** (mutant off) — it calls the real production fn.

```bash
# confirm every target's mutant crashes quickly (fails if any is toothless)
FUZZ_MUTANT=1 ./scripts/fuzz-check.sh

# a single mutant, by hand
RUSTFLAGS='--cfg fuzz_mutant' cargo +nightly fuzz run may_rejoin corpus/may_rejoin -- -runs=100000
```
