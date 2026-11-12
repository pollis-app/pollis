# HPKE golden fixture (suite 0x0052)

One MLS group on `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44`, recorded by the
crypto provider every Pollis build before #1846 links: upstream `openmls_rust_crypto` 0.5.1 at
openmls `34222ef632c87df58baaa7614c8a72ac235c5f07` (hpke-rs 0.7). It is replayed with the
current provider by `hpke_golden_group_from_hpke_rs_0_7_replays`
(`pollis-core/src/commands/mls/tests.rs`).

| File | What it is |
|---|---|
| `joiner_storage_0.json` | Bob's only `mls_kv` row after he built his KeyPackage: the bundle with his init and leaf private keys (openmls's own serde_json); its storage key is in `golden.json` |
| `welcome.bin` | Alice adds Bob: Welcome HPKE-sealed to his init key |
| `commit_1_external.bin` | Carol joins by external commit (HPKE export against the derived `external_pub`, plus an UpdatePath) |
| `commit_2_alice_update.bin` | Alice's path commit; Bob's path secret sealed to his KeyPackage leaf key |
| `commit_3_carol_update.bin` | Carol's path commit; sealed to the parent node whose key Bob derived from commit 2 |
| `application.bin` | Carol's application message carrying `golden.json`'s `plaintext` |
| `golden.json` | suite, group id, exporter label/context/length, and the exporter secret and epoch authenticator at epochs 1 to 4 as the old provider computed them |

All messages are TLS-serialised `MlsMessageOut`. The bytes are frozen: the point is that the
provider that wrote them is not the one reading them, so do not regenerate them with the
current tree. Removing hpke-rs's `draft-connolly-cfrg-hpke-mlkem` feature from
`patches/openmls-rust-crypto` makes the test fail at the Welcome (`PublicKeyMismatch`).

## How it was generated

`generate.rs` is the generator, kept for provenance (cargo does not build it). It ran as the
`main.rs` of a standalone scratch crate depending on `openmls` 0.8
(`draft-ietf-mls-pq-ciphersuites`, `0-8-1-storage-format`), `openmls_traits`,
`openmls_memory_storage`, `tls_codec`, `hex`, `serde_json`, and `openmls_rust_crypto` /
`openmls_basic_credential` as git dependencies at the rev above, with a `[patch.crates-io]` on
the three `openmls*` crates to the same rev. `cargo run --release -- <out-dir>`. Before writing,
it replays Bob from the dumped storage on the old provider and asserts every recorded value.
