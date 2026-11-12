# Narration script — "Binary transparency: payload, signed, and exe" (Topic 10, #599)

No audio is recorded yet (by decision — added later). This script is the single
source of truth for the on-page transcript, the `.vtt` caption track
(`website/learn/media/binary-transparency.vtt`), and future voiceover.

Timings are the caption cue windows and match the scene beats in
`learn/manim/scenes/binary_transparency.py`. Total ≈ 3:40.

---

**[00:00–00:12] Scene 1 — the OS check**
Your operating system checks that an app is signed before it runs it. That check
proves less than you'd think.

**[00:12–00:24] Scene 1 — what a signature proves**
A signature proves the company's key produced these bytes. It says nothing about
what the bytes do. A backdoored build, signed by us, under pressure, passes every
check your computer makes.

**[00:24–00:35] Scene 1 — so we publish**
So we publish the fingerprint of every desktop release, and every over-the-air
mobile update, in public. The question
changes. Not "did they sign it?" but "is this the app they gave everyone, or one
made just for me?"

**[00:35–00:50] Scene 2 — the payload layer**
Each release goes in at three layers, because "the app" isn't one file. The
payload: the compiled program before any signature. That's the layer someone else
can rebuild from source and compare.

**[00:50–01:05] Scene 2 — the signed layer**
The signed layer: what you actually download, after notarization. Signing embeds
unique data on purpose, so this can never be byte-identical twice. We log it
separately, tied to its payload.

**[01:05–01:15] Scene 2 — the third layer**
And the third layer exists because of what a running app can actually measure.

**[01:15–01:32] Scene 3 — what a payload is**
The in-app "verify this build" button needs something to compare the running app
against. But payload is a fingerprint of a whole directory, or of an installer
file. An app that's already installed has neither. It's just… itself.

**[01:32–01:45] Scene 3 — the right comparison**
So an installed app can never match its payload fingerprint, even when it's
genuine. The check needs the right thing to compare against.

**[01:45–02:00] Scene 3 — the third layer**
That's the third layer: the fingerprint of the main program exactly as
installed, the one thing a running app can measure. And releases from before that
layer have nothing to compare against, so the app says "can't check", not "you've
been tampered with".

**[02:00–02:20] Scene 4 — two downloads**
Here's what it buys you. Two people download the app. One gets the public build,
its fingerprint's in the log. One gets a special build, just for them.

**[02:20–02:40] Scene 4 — no quiet option**
Either its fingerprint isn't in the log, and their own app notices, or we put it
in the log, where it sits in public, permanently, next to the real release.
There's no quiet option.

**[02:40–02:55] Scene 5 — the honest part**
Now the part to be careful about. A fingerprint proves we published these bytes.
It does not prove they came from the source code you can read. For that you need
reproducible builds, anyone compiles the source and gets byte-identical output.

**[02:55–03:10] Scene 5 — Linux**
On Linux, we have that. A rebuild that uses none of our secrets, and that anyone
can run from their own copy, rebuilds the AppImage and confirms the hash we logged.

**[03:10–03:22] Scene 5 — macOS, Windows, and signing**
On macOS and Windows, the log proves you got the bytes we published, and those
builds are not rebuilt from source. And the signing layer is never reproducible
anywhere. That's by design: signing mixes in unique data.

**[03:22–03:40] Scene 6 — the second leg**
There's one more leg. Every release also carries a provenance record in a public
log that isn't ours, proving which workflow produced the bytes, with no key of
ours involved in checking it. A different question from reproducibility.
