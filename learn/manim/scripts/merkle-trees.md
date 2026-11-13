# Narration script — "Merkle trees and append-only logs" (Topic 7, #596)

No audio is recorded yet (by decision — added later). This script is the single
source of truth for three artifacts that ship now:

1. the on-page **transcript** (`website/learn.html`, Merkle section),
2. the **`.vtt` caption track** (`website/learn/media/merkle-trees.vtt`), timed to
   the beats below,
3. future voiceover, when we record it.

Timings are the caption cue windows and match the scene beats in
`learn/manim/scenes/merkle_trees.py`. Total ≈ 2:30.

---

**[00:00–00:12] Scene 1 — the problem**
Say we publish a list, every key we've ever issued, and promise we'll only ever
add to it. Never edit. Never delete. How could you check the list was never
changed?

**[00:12–00:24] Scene 1 — the naive answer fails**
You could download the whole list every day and compare. But it grows forever, and
it only works if you never miss a day.

**[00:24–00:40] Scene 2 — build the tree**
Here's the real answer. Hash each entry. Take those fingerprints in pairs, and
hash each pair together. Half as many. Do it again. And again.

**[00:40–00:52] Scene 2 — the root**
Until there's one left, the root. This is the actual root of one of our live logs,
when this video was made: sixty entries, one fingerprint. It has grown since.

**[00:52–01:12] Scene 3 — the ripple**
Watch what happens if we change one character in one entry. Its fingerprint
changes, which changes its parent's, which changes the next one up, and the root
is completely different.

**[01:12–01:26] Scene 3 — why it's unforgeable**
There's no way to edit an entry and keep the root the same. That would mean two
different things with the same fingerprint, the exact thing these functions
prevent.

**[01:26–01:44] Scene 4 — the leverage**
So the root is a fingerprint of the entire list, in sixty-four characters. You
don't download fifty thousand entries to check we've been honest, you compare
sixty-four characters.

**[01:44–02:04] Scene 5 — what it proves**
Now what it proves, and what it doesn't. A Merkle tree doesn't stop us writing something false. Add a
dishonest entry, and the tree would include it quite happily.

**[02:04–02:22] Scene 5 — what it *does* prevent**
What it takes away is our ability to lie quietly. We can't show you one list and
someone else a different one, or change our story later, without it showing the
moment anyone compares roots.

**[02:22–02:30] Scene 5 — close**
That's a smaller claim than "trust us to be honest." It's also a much better one,
because you don't have to. Try it below: change any entry, and watch the root
move.
