# Narration script — "pollis-verify: verify it yourself" (Topic 11, #600)

No audio is recorded yet (by decision — added later). This script is the single
source of truth for the on-page transcript, the `.vtt` caption track
(`website/learn/media/verify-it-yourself.vtt`), and future voiceover.

Timings are the caption cue windows and match the scene beats in
`learn/manim/scenes/verify_it_yourself.py`. Total ≈ 2:45.

---

**[00:00–00:15] Scene 1 — barely an API**
Everything we've talked about is checkable. Here's how, in about two minutes.
First, the API — and it's barely an API. It's a shelf of static files. No
database, no logic, nothing that can decide to behave differently depending on
who's asking.

**[00:15–00:30] Scene 1 — why flat files**
That's on purpose. A server that computes an answer can compute a different one
for you. A server handing out files it wrote in advance has a much harder time.

**[00:30–00:45] Scene 2 — fetch the head**
You can fetch any of them right now. Here's the current signed head — root, entry
count, signature.

**[00:45–01:00] Scene 2 — the same root, twice**
And here's the same root on our website. Same sixty-four characters. Two
different places.

**[01:00–01:15] Scene 3 — files vs claims**
But reading files only tells you what we claim. To check the claims, use the
verifier. It's a small program you download — or build yourself, and if you're
seriously auditing us, build it.

**[01:15–01:30] Scene 3 — what it does**
It re-hashes every entry, rebuilds the tree, checks every proof, and verifies the
signature against a key compiled into it. Not fetched. Compiled in. One command.

**[01:30–01:50] Scene 3 — reading the output**
Watch what comes back. Tree size, sixty. The root — the same one we just fetched.
Then every artifact in the release, each with a tick. Those ticks aren't a status
we wrote down: each one is an inclusion proof your machine just recomputed. And
PASS means the whole tree replayed cleanly, every rule holding, under a key we
couldn't swap on you.

**[01:50–02:20] Scene 4 — the pinned key, again**
Speaking of which. Suppose we served you our own signing key, with a perfect fake
log to match. The verifier compares it to the key built into it — and stops.

**[02:20–02:35] Scene 5 — compare notes**
One last thing, and it's the most important use of this tool. Two people,
different countries, different networks, same command. Same root. Same PASS.

**[02:35–02:45] Scene 5 — the finding that matters**
If those ever differed — two valid signatures over two different trees — that's
the finding that matters. It's also the only one you can't get alone. So run it.
And compare with someone.
