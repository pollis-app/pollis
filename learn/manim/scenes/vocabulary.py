"""
Topic 2, "Keys, encryption, signatures, and hashes, the vocabulary" (#591).

One continuous ~2:00 scene, four vignettes, NO audio (added later). Hard
constraint from the ticket: teach four concepts with NO mathematics on screen at
any point, physical objects only, and no "nearly correct" decryption state,
because that teaches exactly the wrong intuition.

Render (see learn/manim/render.sh):
    learn/manim/render.sh Vocabulary vocabulary m

Accuracy anchors:
  - pollis-core/src/commands/auth.rs, Ed25519 signing keys in the OS keystore
  - pollis-core/src/commands/mls/ds_client.rs, device-signed writes (X-Pollis-*)
  - verifiable-log/src/merkle.rs, SHA-256 is the hash used in the logs
  - pollis-core/src/commands/transparency.rs, the pinned log key 175ebfef…7148
"""

from manim import (
    DOWN,
    LEFT,
    RIGHT,
    UP,
    AddTextLetterByLetter,
    Create,
    FadeIn,
    FadeOut,
    Indicate,
    Line,
    RoundedRectangle,
    Scene,
    Text,
    VGroup,
    Write,
    config,
)

BG = "#0f1117"
FG = "#e4e4e7"
MUTED = "#a1a1aa"
AMBER = "#fdba74"
DANGER = "#f1707b"
OK = "#7bd88f"
LINE = "#3f3f46"
MONO = "Cascadia Code, DejaVu Sans Mono, monospace"

config.background_color = BG

# A real Ed25519 public key from the repo (the pinned transparency-log key), used
# only to show what "a very large number, written in hex" actually looks like.
REAL_KEY = "175ebfef98fc6b20c67c4cba9d4a36a4f85f05afa4e31f707e7d7e3c02227148"


def panel(width, height, color=LINE, fill_opacity=1.0, fill=BG):
    return RoundedRectangle(
        corner_radius=0.1, width=width, height=height,
        stroke_color=color, stroke_width=1.8,
        fill_color=fill, fill_opacity=fill_opacity,
    )


def box(text, color=FG, w=4.0, h=0.9, scale=0.36, mono=True):
    b = panel(w, h, color)
    kw = {"font": MONO} if mono else {}
    t = Text(text, color=color, **kw).scale(scale).move_to(b.get_center())
    g = VGroup(b, t)
    g.box = b
    g.label = t
    return g


class Vocabulary(Scene):
    def construct(self):
        self.beat_key()
        self.beat_lock()
        self.beat_two_keys()
        self.beat_hash()

    def hold_until(self, target):
        """Pad to `target` seconds so each beat stays readable without audio and
        matches learn/manim/scripts/vocabulary.md."""
        now = self.renderer.time
        if target > now:
            self.wait(target - now)

    # ── Scene 1 (0:00–0:20), a key is a very large number ──────────────────
    def beat_key(self):
        head = Text("a key is a very large number", color=AMBER).scale(0.6)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        k = box(REAL_KEY[:32], AMBER, w=11.0, h=1.0, scale=0.44)
        k.move_to([0, 1.5, 0])
        k2 = Text(REAL_KEY[32:], font=MONO, color=AMBER).scale(0.44)
        k2.next_to(k, DOWN, buff=0.25)
        self.play(FadeIn(k), FadeIn(k2))
        real = Text("(that's a real one, the key our transparency log signs with)",
                    color=MUTED).scale(0.36).next_to(k2, DOWN, buff=0.3)
        self.play(FadeIn(real))

        # Scale, as two bars, the second one just leaves the frame.
        bar1 = panel(2.2, 0.4, FG, fill_opacity=0.5, fill=FG).move_to([-3.2, -1.4, 0])
        lab1 = Text("guesses per second, by every computer on earth",
                    color=MUTED).scale(0.32).next_to(bar1, DOWN, buff=0.18)
        self.play(Create(bar1), FadeIn(lab1))

        bar2 = panel(13.5, 0.4, DANGER, fill_opacity=0.5, fill=DANGER)
        bar2.move_to([1.0, -2.5, 0])
        lab2 = Text("guesses needed →  (this bar does not stop at the edge of the screen)",
                    color=DANGER).scale(0.32).next_to(bar2, DOWN, buff=0.18)
        self.play(Create(bar2), FadeIn(lab2))
        note = Text("not 'hard to guess'. there is not enough time or energy in the universe.",
                    color=FG).scale(0.42).move_to([0, -3.6, 0])
        self.play(FadeIn(note))
        self.hold_until(20)
        self.play(FadeOut(VGroup(head, k, k2, real, bar1, lab1, bar2, lab2, note)))

    # ── Scene 2 (0:20–0:40), encryption is a lock that scrambles ───────────
    def beat_lock(self):
        head = Text("encryption is a lock that scrambles",
                    color=AMBER).scale(0.6).to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        msg = box('"see you at 7"', FG, w=4.2, h=0.9)
        msg.move_to([-4.4, 1.6, 0])
        key = box("+ your key", OK, w=3.2, h=0.9)
        key.move_to([0, 1.6, 0])
        noise = box("8f3a9c0d…41be7712", MUTED, w=4.6, h=0.9)
        noise.move_to([4.6, 1.6, 0])
        arrows = VGroup(
            Line(msg.box.get_right(), key.box.get_left(), stroke_color=LINE, stroke_width=2),
            Line(key.box.get_right(), noise.box.get_left(), stroke_color=LINE, stroke_width=2),
        )
        self.play(FadeIn(msg), FadeIn(key), Create(arrows), FadeIn(noise))
        self.hold_until(28)

        right = box("noise + the RIGHT key", OK, w=6.0, h=0.9).move_to([-3.3, -0.4, 0])
        right_out = box('"see you at 7"', OK, w=4.4, h=0.9).move_to([3.6, -0.4, 0])
        a1 = Line(right.box.get_right(), right_out.box.get_left(),
                  stroke_color=OK, stroke_width=2)
        self.play(FadeIn(right), Create(a1), FadeIn(right_out))

        wrong = box("noise + the WRONG key", DANGER, w=6.0, h=0.9).move_to([-3.3, -1.9, 0])
        wrong_out = box("c41d…09aa   (still noise)", DANGER, w=6.0, h=0.9)
        wrong_out.move_to([3.6, -1.9, 0])
        a2 = Line(wrong.box.get_right(), wrong_out.box.get_left(),
                  stroke_color=DANGER, stroke_width=2)
        self.play(FadeIn(wrong), Create(a2), FadeIn(wrong_out))
        none = Text("there is no 'almost'. no partial credit. no 80% of the message.",
                    color=FG).scale(0.46).move_to([0, -3.2, 0])
        self.play(FadeIn(none))
        self.hold_until(40)
        self.play(FadeOut(VGroup(head, msg, key, noise, arrows, right, right_out, a1,
                                 wrong, wrong_out, a2, none)))

    # ── Scene 3 (0:40–1:20), the two-key trick, and signatures ─────────────
    def beat_two_keys(self):
        head = Text("the trick: your key comes in two different halves",
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        pub = box("PUBLIC half, hand it to anyone", OK, w=6.4, h=1.0, scale=0.38, mono=False)
        pub.move_to([-3.5, 2.0, 0])
        priv = box("PRIVATE half, never leaves your device", AMBER, w=6.4, h=1.0,
                   scale=0.38, mono=False)
        priv.move_to([3.5, 2.0, 0])
        self.play(FadeIn(pub), FadeIn(priv))

        # A stranger locks something and then can't open it.
        stranger = box("a stranger, using your public half", MUTED, w=6.6, h=0.9,
                       scale=0.36, mono=False).move_to([-3.5, 0.5, 0])
        locked = box("▓▓▓▓▓▓▓▓▓▓  locked to you", OK, w=6.0, h=0.9).move_to([3.5, 0.5, 0])
        self.play(FadeIn(stranger))
        self.play(FadeIn(locked, shift=RIGHT * 0.25))
        cant = Text("…and now they cannot open it themselves.",
                    color=DANGER).scale(0.46).move_to([0, -0.55, 0])
        self.play(FadeIn(cant))
        only = Text("only the private half opens it, and you never agreed on a secret first.",
                    color=OK).scale(0.44).move_to([0, -1.25, 0])
        self.play(FadeIn(only))
        self.hold_until(65)

        # Backwards: signatures.
        self.play(FadeOut(VGroup(stranger, locked, cant, only)))
        rev = Text("run it backwards and you get signatures",
                   color=AMBER).scale(0.5).move_to([0, 0.7, 0])
        self.play(FadeIn(rev))
        doc = box("a document + your PRIVATE half", AMBER, w=6.6, h=0.9, scale=0.36,
                  mono=False).move_to([-3.5, -0.5, 0])
        seal = box("sealed: provably from you", OK, w=6.0, h=0.9, scale=0.36,
                   mono=False).move_to([3.5, -0.5, 0])
        self.play(FadeIn(doc), FadeIn(seal))
        check = Text("anyone with your public half can check the seal",
                     color=MUTED).scale(0.42).move_to([0, -1.6, 0])
        self.play(FadeIn(check))
        broke = box("change one word → the seal breaks", DANGER, w=8.0, h=0.9,
                    scale=0.4, mono=False).move_to([0, -2.7, 0])
        self.play(FadeIn(broke))
        use = Text("pollis signs every write from your device, and every transparency-log head",
                   color=MUTED).scale(0.36).move_to([0, -3.6, 0])
        self.play(FadeIn(use))
        self.hold_until(80)
        self.play(FadeOut(VGroup(head, pub, priv, rev, doc, seal, check, broke, use)))

    # ── Scene 4 (1:20–2:00), a hash is a fingerprint ───────────────────────
    def beat_hash(self):
        head = Text("a hash is a fingerprint for data",
                    color=AMBER).scale(0.6).to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        # Real SHA-256 digests of these two strings, check them with sha256sum.
        a_in = box('"see you at 7"', FG, w=5.4, h=0.9).move_to([-3.6, 1.7, 0])
        a_out = box("aa0cc4…41f0", OK, w=5.0, h=0.9).move_to([3.6, 1.7, 0])
        arr1 = Line(a_in.box.get_right(), a_out.box.get_left(),
                    stroke_color=LINE, stroke_width=2)
        self.play(FadeIn(a_in), Create(arr1), FadeIn(a_out))
        same = Text("same input → same fingerprint, every time, forever",
                    color=MUTED).scale(0.4).move_to([0, 0.75, 0])
        self.play(FadeIn(same))
        self.hold_until(92)

        b_in = box('"see you at 8"', DANGER, w=5.4, h=0.9).move_to([-3.6, -0.4, 0])
        b_out = box("529b5b…9dea", DANGER, w=5.0, h=0.9).move_to([3.6, -0.4, 0])
        arr2 = Line(b_in.box.get_right(), b_out.box.get_left(),
                    stroke_color=DANGER, stroke_width=2)
        self.play(FadeIn(b_in), Create(arr2), FadeIn(b_out))
        one = Text("one character changed → completely different. not slightly. completely.",
                   color=FG).scale(0.44).move_to([0, -1.5, 0])
        self.play(FadeIn(one))
        try_it = Text("(there's a box below this video, type in it and watch)",
                      color=AMBER).scale(0.4).move_to([0, -2.2, 0])
        self.play(FadeIn(try_it))
        self.hold_until(108)

        self.play(FadeOut(VGroup(a_in, a_out, arr1, same, b_in, b_out, arr2, one, try_it)))
        close = VGroup(
            Text("key · encryption · signature · hash", color=AMBER).scale(0.6),
            Text("everything else in this section is those four ideas, arranged.",
                 color=FG).scale(0.44),
            Text("and the maths is not the weak point, it essentially never is.",
                 color=MUTED).scale(0.42),
            Text("where keys are kept · whether a public key is really theirs · "
                 "whether the app is honest", color=MUTED).scale(0.36),
        ).arrange(DOWN, buff=0.3).move_to([0, 0.2, 0])
        for ln in close:
            self.play(FadeIn(ln, shift=UP * 0.12), run_time=0.4)
        self.hold_until(122)
        self.play(FadeOut(VGroup(head, close)))
        self.wait(0.5)
