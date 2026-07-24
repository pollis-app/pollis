# Narration script — "Threat model: who can see what" (Topic 1, #590)

No audio is recorded yet (by decision — added later). This script is the single
source of truth for the on-page transcript, the `.vtt` caption track
(`website/learn/media/threat-model.vtt`), and future voiceover.

Timings are the caption cue windows and match the scene beats in
`learn/manim/scenes/threat_model.py`. Total ≈ 2:15.

---

**[00:00–00:12] Scene 1 — trusting ability, not promises**
Most messaging apps ask you to trust a company. Not their promise — their
ability. Even a company that doesn't want to read your messages can be broken
into, bought, or ordered by a court.

**[00:12–00:20] Scene 1 — intent changes**
"We would never" describes intent. Intent changes. So Pollis is built so reading
your messages isn't something we choose not to do. It's something we can't do.

**[00:20–00:45] Scene 2 — sealed envelopes**
Your message is sealed on your device, before it goes anywhere. Our servers pass
it along without ever holding the key. It opens on your friend's device. We're a
post office handling sealed envelopes.

**[00:45–01:00] Scene 3 — the outside of the envelope**
But look closer at the envelope, because this is the part most apps gloss over.
We can't see what's inside. We *can* see the outside.

**[01:00–01:15] Scene 3 — what metadata is**
That your account sent something, when, roughly how big, and which conversation
it belongs to. That's called metadata, and it can be sensitive all by itself.
We're not going to tell you it doesn't exist.

**[01:15–01:30] Scene 4 — the subtler attack**
There's a subtler attack too. Encryption only helps if you're using the right key
for the right person. A dishonest server could hand you the wrong key — its own —
and read everything without ever breaking the maths.

**[01:30–01:45] Scene 4 — the two defences**
That's the attack we take most seriously, and it's why the rest of this section
exists: safety numbers, and public logs of every key we publish. Both are things
you can check yourself.

**[01:45–02:05] Scene 5 — the boundary**
Here's the full picture. Green, we cannot see. Amber, we can — and we say so. Red
is outside what any messenger can defend: your own device, and our own bugs.

**[02:05–02:15] Scene 5 — the pattern**
Everything else in Learn is about shrinking how much of the green depends on
trusting us — and publishing evidence for whatever's left.
