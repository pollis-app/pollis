# Narration script — "Reading the artifacts page and the Security page" (Topic 12, #601)

No audio is recorded yet (by decision — added later). This script is the single
source of truth for the on-page transcript, the `.vtt` caption track
(`website/learn/media/reading-the-dashboards.vtt`), and future voiceover.

Timings are the caption cue windows and match the scene beats in
`learn/manim/scenes/reading_the_dashboards.py`. Total ≈ 3:00.

---

**[00:00–00:15] Scene 1 — the tour begins**
You've got every idea you need to read our dashboards. Let's go through them.
Latest releases — what we currently ship, and where to get it.

**[00:15–00:28] Scene 1 — release proofs**
Release proofs. This is the one that matters. For the current version: is it in
the binaries log, did the tree check out, and what's inside.

**[00:28–00:40] Scene 1 — the rest of the page**
Below that, the daily self-audit: the current head of all three logs. And at the
bottom, the one key everything reduces to.

**[00:40–00:55] Scene 2 — one row, decoded**
Each row is one piece of a release. Platform. Bundle. Layer — payload, signed, or
exe, the three from earlier. The hashes. And a tick.

**[00:55–01:10] Scene 2 — what a tick means**
Read that tick carefully. It doesn't mean someone marked a box. It means an
inclusion proof was recomputed, and it held.

**[01:10–01:20] Scene 2 — pending is not an alarm**
And if it says "not in log yet", that's almost always the daily rebuild. Not an
alarm.

**[01:20–01:50] Scene 3 — the same root, three places**
Tree size and root — that's the signed head. And this root is the same string
you'd get by fetching the file yourself, or by running the verifier. Same
sixty-four characters, three places. Also note what that page says about itself:
the key check in your browser is a convenience, not a trust anchor. Real
verification uses a key you got independently.

**[01:50–02:05] Scene 4 — inside the app**
Now inside the app. "This build" tells you whether the exact binary you're
running is in the public log. Four possible answers, and the difference between
two of them matters a lot.

**[02:05–02:20] Scene 4 — pending and unavailable**
"Pending" means the log hasn't rebuilt yet. "Unavailable" means we couldn't check
— the log was unreachable, or this release is older than the layer that makes
checking possible.

**[02:20–02:32] Scene 4 — the loud one**
And "not in public log" is the loud one: the release is published, there's
something to compare against, and yours isn't it.

**[02:32–02:45] Scene 4 — why the distinction matters**
"Unavailable" versus "not in the log" is the difference between "I don't know"
and "you've been tampered with". We shipped that wrong once, and every honest
macOS build was accused of being fake. It's fixed — and it's exactly why the
distinction is worth labouring.

**[02:45–02:55] Scene 5 — what green means**
Below that: your account key, re-checked against the public log. Your devices.
Your security events. Safety numbers per contact. And here's the honest ending.
Everything green means the things we published are consistent, permanent, and
include the app you're running.

**[02:55–03:00] Scene 5 — and what it doesn't**
It does not mean our code has no bugs. It does not mean your device is safe. And
it does not mean the metadata is invisible. We've shrunk what you have to take on
faith, and published evidence for what's left. Now you can check it.
