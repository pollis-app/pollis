# Narration script — "Joining, leaving, multi-device, and the honest history boundary" (Topic 4, #593)

No audio is recorded yet (by decision — added later). This script is the single
source of truth for the on-page transcript, the `.vtt` caption track
(`website/learn/media/joining-leaving.vtt`), and future voiceover.

Timings are the caption cue windows and match the scene beats in
`learn/manim/scenes/joining_leaving.py`. Total ≈ 2:55.

---

**[00:00–00:25] Scene 1 — key packages**
To add you to a group, someone needs to encrypt its keys to you — even if you're
offline. So every device publishes a small lockbox in advance. Anyone can put
something in. Only you can open it.

**[00:25–00:55] Scene 2 — the Welcome**
When you're added, an existing member seals the group's current state into your
lockbox and sends it over. You open it, and you're in.

**[00:55–01:15] Scene 3 — the part people don't expect**
Now, the part people don't expect. You received the keys as they are *now*. Not
the old ones. MLS is built so that new keys can't be used to work out old ones —
and that's not a limitation, it's the entire reason removing someone actually
works.

**[01:15–01:30] Scene 3 — the boundary**
So messages sent before you joined stay sealed. Watch — it isn't that we're
hiding them. The key genuinely doesn't fit.

**[01:30–01:55] Scene 4 — removal**
Removal is the same idea. This member is removed, the keys along their path are
replaced, and the next message is sealed to them. They still have their old key.
It opens nothing. What they already decrypted stays decrypted — we can't reach
into their machine.

**[01:55–02:20] Scene 5 — your own devices**
Your own devices work exactly the same way. Your phone and your laptop are two
separate members of the group, each with their own keys. Nothing is copied
between them — because a key that travels is a key that can be stolen on the way.
And a new laptop added today has the same empty history before today.

**[02:20–02:40] Scene 6 — the trade**
Which brings us to the trade-off we want you to hear from us, not discover later.
A new device starts empty. There's no history backup. Your old messages live on
your old device, and nowhere else. We could do it the common way: encrypt your
history, store it on our servers, protect it with a PIN. Watch what that actually
builds — a copy of every conversation, on our infrastructure, behind a six-digit
number.

**[02:40–02:55] Scene 6 — what we do promise**
That's the exact thing this app exists to not have. So we don't do it. What we
*do* promise is narrow and firm. Two kinds of loss are acceptable: messages from
before you joined, and a new device starting empty. Everything else has to
arrive. Anything else going missing is a bug, and we treat it as one.
