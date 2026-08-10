# Runbook: the transparency-log signing key is compromised

> What to do when the online signing key (`STH_SIGNING_KEY`) leaks, or you have
> reason to think it might have. Implements the recovery path from #754.

## Why this document exists

Before root-signed key sets, this situation had no clean answer. The client pinned
the signer's public key as a compiled-in constant, so retiring a leaked key meant
shipping a client update — and that update was verified through the binaries tree
**the leaked key signs**. An attacker holding the key could vouch for their own build
of the fix. The remedy depended on the thing that was broken.

Now the client pins an **offline root** instead. The root signs nothing but key-set
statements — "these key ids may sign these trees, until this date" — so retiring a
signer is a statement the root makes, and clients accept it **without a release**.

That is the whole point, and it is the sentence to check this runbook against: if a
step here requires shipping a binary, the design has failed.

## Assume compromise if any of these is true

- The `STH_SIGNING_KEY` secret was read, printed, or copied out of GitHub Actions.
- A workflow change, dependency, or third-party Action gained access to it.
- Someone with repository admin left, or an admin account was compromised.
- A tree head verifies against the key but nobody published it (an equivocation
  alarm from a monitor or the in-app check).

You do **not** need proof. Rotating costs a ceremony and a publish; not rotating
when you should have costs the log's meaning.

## Recovery

**1. Mint a new signer.** On any machine — this one is online by design.

```
cargo build -q -p verifiable-log-builder --bin builder
./target/debug/builder keygen
```

Put the new seed in `STH_SIGNING_KEY` (GitHub Actions secret). Keep the public half.

**2. Sign a key set that retires the old signer.** On the machine holding the
**offline root**, never in CI.

```
./target/debug/builder key-set \
  --issued-at   <now-ms> \
  --not-after   <now-ms + 1 year> \
  --signer "<NEW_PUBLIC_KEY>:pollis-verifiable-log:sth:v2,pollis-verifiable-log:sth:v2:account-keys,pollis-verifiable-log:sth:v2:binaries" \
  --out key-set.json
```

**Omit the compromised key entirely.** Do not give it a short `not_after` — an
attacker who can delay a client's fetch keeps it alive for exactly that long.
Omission takes effect the moment a client sees the new statement.

**3. Publish.** Commit `key-set.json` and run `transparency-publish.yml`. The publish
verifies the statement against the pinned root first, so a mistake here fails the
job rather than reaching clients.

**4. Re-sign the trees** with the new key. The next scheduled run does this; dispatch
it if you do not want to wait.

**No client update is required.** Clients fetch the new statement, verify it against
the root they already pin, and stop accepting the retired signer.

## What an attacker can still do

Be honest about the bound. Holding the leaked signer key, before clients see the new
statement, an attacker can sign a tree head that verifies. What they cannot do is:

- **authorise themselves** — the key set is signed by a root they do not hold;
- **outlast the retirement** — omission is not a timer they can wait out;
- **suppress the fix** — the recovery path does not run through the tree they can sign.

The residual exposure is the window between the leak and clients fetching the new key
set, bounded by the statement's own `not_after`. Shorter statements shrink the
window; they also mean a client offline longer than that window reports "cannot
verify" rather than trusting a stale set. A year is the current choice.

## If the ROOT is compromised

This is the bad one, and it is why the root lives offline and signs rarely.

There is no in-band recovery: clients pin the root, so replacing it **does** require a
client release, and that release's provenance rests on the binaries tree. The
sequence is: mint a new root, ship a client pinning both (the outgoing one with the
end of its overlap window), wait out the overlap, then publish a statement signed by
the new root alone.

Which is to say: root compromise puts you back where signer compromise used to be.
That asymmetry is the argument for keeping the root offline, on a machine that never
holds a CI token, and for it signing nothing but key sets.

## Related

- `docs/sth-signing-key-custody.md` — §4 custody decision, §6 anchor split.
- `verifiable-log/src/keyset.rs` — the format and its verification rules.
- `pollis-core/src/commands/transparency.rs` — `PINNED_LOG_ROOT_KEYS`, `signers_from_key_set`.
