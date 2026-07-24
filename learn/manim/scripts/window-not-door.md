# Narration script — "Forward secrecy and post-compromise security" (Topic 5, #594)

No audio is recorded yet (by decision — added later). This script is the single
source of truth for the on-page transcript, the `.vtt` caption track
(`website/learn/media/window-not-door.vtt`), and future voiceover.

Timings are the caption cue windows and match the scene beats in
`learn/manim/scenes/window_not_door.py`. Total ≈ 2:50.

---

**[00:00–00:20] Scene 1 — assume the worst**
Let's assume the worst has already happened. Someone has a key from your device.
Today. What did they just get? Two questions: what about the past, and what about
the future.

**[00:20–00:35] Scene 2 — the past**
The past first. They try yesterday's messages — and get nothing.

**[00:35–00:55] Scene 2 — why**
Here's why. Keys move forward in one-way steps. Each key is made from the one
before it, through a process that can't be reversed. Like mixing paint: easy
forwards, impossible backwards. And once a key is used, it's deleted. Yesterday's
key doesn't exist anymore — not on your device, not on our servers, not anywhere.

**[00:55–01:20] Scene 3 — the caveat**
One honest caveat, because this gets overstated. If your old messages are still
on your screen, someone holding your unlocked device just reads them. Forward
secrecy protects encrypted traffic — someone who recorded your data months ago
and steals a key today still can't open what they recorded. That's the real
promise, and it's a good one.

**[01:20–01:40] Scene 4 — a real break**
Now the future — and this is the part I find genuinely surprising. Normally a
stolen key means you're compromised forever. Here, the attacker is reading. Epoch
seven. Epoch eight. This is a real break.

**[01:40–02:00] Scene 4 — the group heals**
Then someone else in the group — someone the attacker doesn't control — does
something completely ordinary. Adds a member, or just refreshes their keys. New
keys flow up the path. Epoch nine arrives. And the attacker is out. Nobody
detected anything. Nobody responded to an incident. The group healed by carrying
on normally.

**[02:00–02:25] Scene 5 — the window**
That's post-compromise security, and it's why the tree was worth the trouble.
Sealed on the left by forward secrecy. Sealed on the right by the next commit. A
stolen key is a window, not a door — and the more the group talks, the narrower it
gets.

**[02:25–02:50] Scene 6 — the limits**
The limit: if they never lose access to your device, they get every update you do,
and they stay in. Anything already decrypted on your device is readable by whoever
holds it. Neither property touches metadata. And healing is driven by activity — a
silent group rekeys rarely. This recovers from a break that ended. It doesn't fix
one that's still happening.
