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

Pinned transparency-log public key (Ed25519):
`175ebfef98fc6b20c67c4cba9d4a36a4f85f05afa4e31f707e7d7e3c02227148`
