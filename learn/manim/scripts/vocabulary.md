# Narration script — "Keys, encryption, signatures, and hashes" (Topic 2, #591)

No audio is recorded yet (by decision — added later). This script is the single
source of truth for the on-page transcript, the `.vtt` caption track
(`website/learn/media/vocabulary.vtt`), and future voiceover.

Timings are the caption cue windows and match the scene beats in
`learn/manim/scenes/vocabulary.py`. Total ≈ 2:05.

---

**[00:00–00:20] Scene 1 — a key is a number**
A key is a very large number. That's genuinely all it is. Large enough that
guessing it isn't hard — it's impossible, in the sense that there isn't enough
time or energy in the universe to try.

**[00:20–00:40] Scene 2 — encryption**
Encryption is a lock. Your message goes in with a key, and what comes out is
noise. Put the noise back in with the right key, and your message returns. Wrong
key, and you get nothing — not a blurry version, not most of it. Nothing.

**[00:40–01:05] Scene 3 — the two-key trick**
Now the idea that makes all of this work between strangers. Your key comes in two
halves that are different from each other. One is public — you hand it to anyone,
publish it, put it on a billboard. One is private, and it never leaves your
device. Anything locked with the public half can only be opened by the private
half. A stranger can lock a message to you, and then cannot unlock it themselves.

**[01:05–01:20] Scene 3 — signatures**
Run the same trick backwards and you get signatures. Stamp something with your
private half, and anyone with your public half can confirm it came from you and
hasn't been altered. Change one word, and the seal breaks.

**[01:20–01:35] Scene 4 — a hash**
Last idea: a hash. It's a fingerprint for data. Any file, any size, in — a short
string out. Same file, same fingerprint, every time.

**[01:35–01:50] Scene 4 — change one character**
But change one single character… and it's completely different. Not slightly.
Completely. Try it yourself in the box below.

**[01:50–02:05] Scene 4 — the close**
Key, encryption, signature, hash. Everything else in this section is those four
ideas, arranged. One last thing: the maths is not the weak point — it essentially
never is. The weak points are where the keys are kept, whether you believe a
public key really belongs to who it says, and whether the app doing the locking
is honest. That's what the rest of Learn is about.
