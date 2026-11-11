# Narration script — "Signed tree heads, proofs, and equivocation" (Topic 8, #597)

No audio is recorded yet (by decision — added later). This script is the single
source of truth for three artifacts that ship now:

1. the on-page **transcript** (`website/learn.html`, Topic 8 section),
2. the **`.vtt` caption track**
   (`website/learn/media/proofs-and-equivocation.vtt`), timed to the beats below,
3. future voiceover, when we record it.

Timings are the caption cue windows and match the scene beats in
`learn/manim/scenes/proofs_and_equivocation.py`. Total ≈ 3:05.

---

**[00:00–00:14] Scene 1 — two questions**
You've got a root, sixty-four characters standing in for an entire list. Two
questions follow. First: is my entry actually in there?

**[00:14–00:32] Scene 1 — the audit path**
You don't need the whole list. You need the siblings along the path from your
entry up to the top. Hash yours. Combine it with this sibling. Hash. Combine with
the next one. Climb.

**[00:32–00:44] Scene 1 — match or don't**
If you land on the published root, your entry is in the tree. If you land anywhere
else, it isn't. Here's a forged entry doing exactly that, a different root.

**[00:44–00:52] Scene 1 — the scaling**
A million entries needs about twenty hashes. Double the list, add one step. That's
why this works for logs that grow forever.

**[00:52–01:10] Scene 2 — consistency**
Second question: is this the same list as last week, with things only added? A
consistency proof shows the old tree sitting inside the new one, untouched, with
the new entries added on the end.

**[01:10–01:26] Scene 2 — the cheat fails**
Now watch what a tamper attempt looks like. We edit an old entry. The old tree no
longer fits inside the new one. No proof exists. We can't manufacture one.

**[01:26–01:35] Scene 2 — the turn**
So "append-only" isn't a promise. It's something you check.

**[01:35–01:50] Scene 3 — the signed tree head**
But who says which root is real? We sign it, the root, the count, the time, all
sealed together. That's a signed tree head.

**[01:50–02:05] Scene 3 — the pinned key**
And the key we sign with is baked into the app and the verifier. Not downloaded.
If it were fetched, a dishonest server could hand you its own key with a perfect
fake log attached, and everything would verify.

**[02:05–02:25] Scene 4 — two sets of books**
Which leaves one last move, the one worth understanding. Different people could be
shown different lists. A clean list for the auditors, and a slightly different one
for a single person.

**[02:25–02:42] Scene 4 — both verify**
Both append-only. Both signed. Both verify perfectly. Neither of you can tell
alone.

**[02:42–02:57] Scene 4 — compare**
So compare. Two signed heads, side by side. They disagree, and both carry our
signature. There's no explanation for that. It's permanent, public proof we
cheated.

**[02:57–03:05] Scene 5 — the chain**
Root. Inclusion. Consistency. Signature. Comparison. Each one closes a door.
