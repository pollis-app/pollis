# Security Policy

## Reporting a vulnerability

Report vulnerabilities privately via [GitHub private vulnerability reporting](https://github.com/actuallydan/pollis/security/advisories/new).
Please do not open public issues for security problems.

A useful report names the affected component, the impact, and steps to
reproduce. You'll get a response through the advisory thread.

## Scope

- This repository — desktop app, `pollis-core`, `pollis-delivery`, the
  transparency-log toolchain (`verifiable-log*`)
- The delivery service at `api.pollis.com`
- The public transparency log at `verify.pollis.com`

## Verifying Pollis yourself

Every release is recorded in a public append-only transparency log and carries
keyless cosign / SLSA build provenance. To check our work from your own
machine, start at [docs/verify-transparency-log.md](docs/verify-transparency-log.md).

Pinned transparency-log public key (ML-DSA-44): **being rotated** (#668). The
log's Ed25519 key is retired; the new post-quantum key is published here, in the
`pollis-verify` release body, and on <https://pollis.com/artifacts.html> once
every tree has been republished under it. Until then no build pins a key, and
`pollis-verify` reports *unverified* — it never falls back to trusting whatever
the server serves.
