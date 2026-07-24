# Narration script — "Identity keys, safety numbers, and TOFU pinning" (Topic 6, #595)

No audio is recorded yet (by decision — added later). This script is the single
source of truth for the on-page transcript, the `.vtt` caption track
(`website/learn/media/identity-keys.vtt`), and future voiceover.

Timings are the caption cue windows and match the scene beats in
`learn/manim/scenes/identity_keys.py`. Total ≈ 2:45.

---

**[00:00–00:15] Scene 1 — the gap**
Encryption protects the message. It doesn't tell you who's on the other end. When
you first message someone, your app asks our servers for their public key. And
that's the gap.

**[00:15–00:35] Scene 1 — the attack, working**
Watch. Alice asks for Bob's key. We hand her one labelled "Bob" — but it's ours.
Now everything Alice sends opens here, gets read, and gets re-sealed for Bob. Look
at their screens. Padlocks. Both of them. Nothing looks wrong, because nothing
*is* wrong with the encryption. It's working perfectly — on the wrong key.

**[00:35–00:55] Scene 2 — safety numbers**
Every end-to-end encrypted app has this problem. Here are the two answers. First:
safety numbers. Your device combines your identity key with your contact's and
produces a short code. They do the same. Same two keys, same code — it matches.

**[00:55–01:10] Scene 2 — under attack**
Now run the attack again. Alice's device is mixing in our key, not Bob's. Bob's is
mixing in his real one. Different inputs, different codes. Compare them any way we
can't interfere with — hold the phones together, read them out on a call, scan the
QR — and the middleman is caught immediately.

**[01:10–01:30] Scene 3 — the catch**
The catch is obvious. It only works if you actually check. And most people never
do. So there's a second answer, and it works even when nobody checks.

**[01:30–01:55] Scene 4 — the log**
Every identity key we publish goes into a public log that only ever grows. To pull
off that attack, we'd have to publish the fake key — here, in the open,
permanently. And watch us try to take it back. We can't. The log doesn't work that
way.

**[01:55–02:15] Scene 4 — your device is watching**
And your own device is reading that log. It compares what the world is being shown
for you against the key you actually hold. If they ever disagree, your app tells
you. That's the shift: the attack stops being invisible and becomes permanent
public proof that we cheated.

**[02:15–02:35] Scene 5 — TOFU**
One last thing. The first time you talk to someone, your app remembers their key.
If it ever changes, you get a banner. Usually that's innocent — a new phone, a
reinstall. Sometimes it isn't. Your app can't tell the difference, which is why it
asks you.

**[02:35–02:45] Scene 5 — close**
Compare safety numbers once, with the people who matter. The log handles the rest.
