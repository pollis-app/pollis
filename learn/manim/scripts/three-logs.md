# Narration script — "Pollis's three logs, and what each one prevents" (Topic 9, #598)

No audio is recorded yet (by decision — added later). This script is the single
source of truth for the on-page transcript, the `.vtt` caption track
(`website/learn/media/three-logs.vtt`), and future voiceover.

Timings are the caption cue windows and match the scene beats in
`learn/manim/scenes/three_logs.py`. Total ≈ 3:00.

---

**[00:00–00:20] Scene 1 — three shelves, one key**
Three logs. One signing key. One address: verify dot pollis dot com.

**[00:20–00:40] Scene 2 — account keys, and the attack it closes**
The first holds every identity key we've ever published. Remember the attack
where we hand you the wrong key? To do that, we'd have to write the fake key
*here* — permanently, in public, where the real owner's own app is looking.

**[00:40–01:00] Scene 2 — the rule**
And the rule on this log is that versions only move forward. We can't slip an old
key back in, or renumber history.

**[01:00–01:15] Scene 3 — the commit log**
The second holds every commit in every conversation. Groups move forward in
epochs, and everyone has to apply them in the same order.

**[01:15–01:32] Scene 3 — the fork attempt**
So watch what a dishonest server would try: two different commits, both claiming
to be epoch eight. The group splits — half in one reality, half in another.

**[01:32–01:45] Scene 3 — the rule, and what's actually stored**
The log's rule makes that impossible. One epoch, one commit. The fork is
rejected, and the attempt is on the record. And look closely at what's stored —
it's sealed. The log publishes the *order* of a conversation. Never a word of its
content.

**[01:45–02:10] Scene 4 — binaries**
The third holds the fingerprint of every app we've ever released. If we ever
built a special version for one person, that fingerprint is either in this public
log — where anyone can see it — or missing from it, where that person's own app
notices. There's no third option.

**[02:10–02:25] Scene 5 — why three logs**
Why three logs instead of one? Because a signed head for one must never work as a
head for another.

**[02:25–02:40] Scene 5 — domain separation**
Take a valid head from the commit log, and try to pass it off as one for the
binaries log. It doesn't fit. Each tree signs under its own context, so the
signature says which tree it belongs to.

**[02:40–02:52] Scene 6 — the honest limit**
These don't make us honest. We could write something dishonest to any of them.
What changes is that it's permanent, public, and identical for everyone who looks
— undeniable, not impossible. They also say nothing about bugs. A log proves what
we published; it doesn't prove it's correct.

**[02:52–03:00] Scene 6 — pending is not an alarm**
One practical note. The logs rebuild once a day. A brand-new release, or a key
published an hour ago, may genuinely not be in there yet. That's why your app says
"pending" instead of raising an alarm.
