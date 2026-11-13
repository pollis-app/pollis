# Narration script — "Joining, leaving, multi-device, and the honest history boundary" (Topic 4, #593)

No audio is recorded yet (by decision — added later). This script is the single
source of truth for the on-page transcript, the `.vtt` caption track
(`website/learn/media/joining-leaving.vtt`), and future voiceover.

Timings are the caption cue windows and match the scene beats in
`learn/manim/scenes/joining_leaving.py`. Total ≈ 2:55.

---

**[00:00–00:25] Scene 1 — key packages**
To add you to a group, someone needs to encrypt its keys to you, even if you're
offline. So every device publishes a small lockbox in advance. Anyone can put
something in. Only you can open it.

**[00:25–00:55] Scene 2 — the Welcome**
When you're added, an existing member seals the group's current state into your
lockbox and sends it over. You open it, and you're in.

**[00:55–01:15] Scene 3 — the part people don't expect**
Now, the part people don't expect. You received the keys as they are now. Not the
old ones. MLS is built so that new keys can't be used to work out old ones, and
that's not a limitation, it's the entire reason removing someone actually works.

**[01:15–01:30] Scene 3 — the boundary**
So messages sent before you joined stay sealed. Watch, it isn't that we're hiding
them. The key genuinely doesn't fit.

**[01:30–01:55] Scene 4 — removal**
Removal is the same idea. This member is removed, the keys along their path are
replaced, and the next message is sealed to them. They still have their old key.
It opens nothing. What they already decrypted stays decrypted, we can't reach into
their machine.

**[01:55–02:20] Scene 5 — your own devices**
Your own devices work exactly the same way. Your phone and your laptop are two
separate members of the group, each with their own keys. Their group keys are never copied
between them. The one key that does travel is your account key, once, sealed end
to end, when you link the new device.

**[02:20–02:40] Scene 6 — the trade**
Which brings us to how history works on a new device.
A new device starts empty, unless you copy your history over from another of
your devices, directly, sealed end to end. There's no history backup on our
servers. We could encrypt your
history and store it on our servers behind a PIN, but watch what that builds: a
copy of every conversation, on our infrastructure, behind a six-digit number.

**[02:40–02:55] Scene 6 — what we do promise**
That's the exact thing this app exists to not have. So we don't do it. Three kinds
of loss are acceptable: messages from before you joined, a new device starting
empty, and — if one of your devices goes quiet for over a year — mail still
waiting only for that device. Everything else has to arrive. Anything else going missing
is a bug.
