#!/usr/bin/env python3
"""Synthesize assets/sfx/ringback.wav, the caller's ringback tone for a ringing DM call (#1901).

Original work, generated here from first principles (no sampled audio), so it carries
the repository's own licence. Re-run to reproduce the committed file byte for byte:

    python3 scripts/gen-ringback-wav.py

The cadence is the familiar "ring-ring": two 0.4 s bursts 0.2 s apart, then 2 s of
silence, built into the clip so Rust's looping playback (`start_ringback`) produces
the pattern by replaying the whole file. The tone is 400 Hz + 450 Hz. The peak sits at
-15 dBFS, the level of the other call sounds in assets/sfx, and every burst fades in
and out over 10 ms so the loop never clicks.
"""

import math
import struct
import wave
from pathlib import Path

RATE = 44_100
FREQS = (400.0, 450.0)
# Per-sine amplitude: the two sines sum to a peak of 2 * AMP, about -15 dBFS.
AMP = 0.09
FADE_S = 0.010
# (seconds, tone on?) in playback order.
CADENCE = ((0.4, True), (0.2, False), (0.4, True), (2.0, False))

OUT = Path(__file__).resolve().parent.parent / "assets" / "sfx" / "ringback.wav"


def burst(seconds: float) -> list[int]:
    n = round(seconds * RATE)
    fade = round(FADE_S * RATE)
    out = []
    for i in range(n):
        t = i / RATE
        env = min(1.0, i / fade, (n - 1 - i) / fade)
        v = AMP * env * sum(math.sin(2 * math.pi * f * t) for f in FREQS)
        out.append(round(v * 32767))
    return out


def main() -> None:
    samples: list[int] = []
    for seconds, on in CADENCE:
        samples += burst(seconds) if on else [0] * round(seconds * RATE)
    with wave.open(str(OUT), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(struct.pack(f"<{len(samples)}h", *samples))
    print(f"wrote {OUT} ({len(samples) / RATE:.1f} s)")


if __name__ == "__main__":
    main()
