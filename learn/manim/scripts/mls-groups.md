# Narration script — "MLS groups: the ratchet tree, epochs, and commits" (Topic 3, #592)

No audio is recorded yet (by decision — added later). This script is the single
source of truth for the on-page transcript, the `.vtt` caption track
(`website/learn/media/mls-groups.vtt`), and future voiceover.

Timings are the caption cue windows and match the scene beats in
`learn/manim/scenes/mls_groups.py`. Total ≈ 2:45.

---

**[00:00–00:15] Scene 1 — the obvious approach**
Two people sharing one key is easy. Groups are where it gets interesting. Here's
the obvious approach: encrypt the message separately for every person. Five
people, four copies.

**[00:15–00:30] Scene 1 — why it doesn't scale**
It works — but watch what happens when the group grows. A hundred people means
ninety-nine copies of every message you send. And when someone leaves, everyone
needs new keys, which means everyone coordinating with everyone.

**[00:30–00:48] Scene 2 — the tree**
MLS arranges the group as a tree instead. Everyone sits at the bottom. Above
them, nodes pair up, all the way to a single root. Every node holds a key — and
here's the rule: you know the keys on the path from your own leaf to the root.

**[00:48–01:00] Scene 2 — the shared part**
The root is known to everyone. That's what protects the group's messages. And
when two members' paths overlap, that shared part is the whole trick.

**[01:00–01:25] Scene 3 — an update ripples**
Now watch what happens when something changes. This member generates fresh keys
along their path — and sends each one only to the sibling branch at that level.
Not to every member. To one branch at each step.

**[01:25–01:45] Scene 3 — the payoff**
Everyone can unwrap what they need, using the part of the tree they already know.
Ninety-nine messages becomes seven. It's the depth of the tree, not the size of
the group.

**[01:45–02:15] Scene 4 — removal**
Removal works the same way, and it's the important case. This member is removed.
New keys flow up the vacated path. The next message goes out — and for them, it
stays sealed. They're in the same room, holding the old key, and it opens
nothing.

**[02:15–02:30] Scene 5 — epochs and commits**
Two words you'll see everywhere in Pollis. An epoch is the group's version number
— it ticks up every time the keys change. A commit is the message that makes it
tick.

**[02:30–02:45] Scene 5 — ordering**
Every member has to apply those commits in the same order. If a device misses
one, it falls out of step and can't read what comes next — until it catches up.
Keeping that order correct, across every device, network drop, and reconnection,
is the hardest problem in this app. It's also why we publish every commit to a
public append-only log.
