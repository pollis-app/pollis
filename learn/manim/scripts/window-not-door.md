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
The past first. They try yesterday's messages, and get nothing.

**[00:35–00:55] Scene 2 — why**
Here's why. Keys move forward in one-way steps. Each key is made from the one
before it, through a process that can't be reversed. Like mixing paint: easy
forwards, impossible backwards. And once a key is used, it's deleted, not on your
device, not on our servers, not anywhere.

**[00:55–01:20] Scene 3 — your device**
One thing to be precise about. If your old messages are still on
your screen, someone holding your unlocked device just reads them. Forward secrecy
protects encrypted traffic, someone who recorded your data months ago and steals a
key today still can't open what they recorded. Shared files are sealed with a file key that members' devices keep for the life of the conversation.

**[01:20–01:40] Scene 4 — a real break**
Now the future. Normally a stolen key means you're compromised forever. Here, the
attacker is reading. Epoch seven. Epoch eight. This is a real break.

**[01:40–02:00] Scene 4 — the group heals**
Then someone else in the group, someone the attacker doesn't control, does
something completely ordinary. New keys flow up the path. Epoch nine arrives. And
the attacker is out. Nobody detected anything. The group healed by carrying on
normally.

**[02:00–02:25] Scene 5 — the window**
Sealed on the left by forward secrecy. Sealed on the right by the next commit. A
stolen key is a window, not a door, and the more often the group's keys change, the narrower it
gets. That's what cheap rekeying bought us.

**[02:25–02:50] Scene 6 — the limits**
The limits: if they never lose access to your device, they get every update you do
and they stay in. Anything already decrypted on your device is readable by whoever
holds it. Neither property touches metadata. This recovers from a break that
ended, not one that's still happening.
