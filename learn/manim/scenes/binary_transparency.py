"""
Topic 10, "Binary transparency: payload, signed, and exe" (#599).

One continuous ~3:40 scene, six beats, NO audio (added later). The `exe`-layer
bug (#587/#588) is told straight, because it is the reason the layer exists; the
reproducibility beat states macOS/Windows as NOT reproducible in the reader's
path, and never implies the signed layer could be.

Render (see learn/manim/render.sh):
    learn/manim/render.sh BinaryTransparency binary-transparency m

Accuracy anchors:
  - verifiable-log-builder/src/binaries.rs → Layer::{Payload,Signed,Exe}
  - scripts/attest-binaries.sh, scripts/lib/payload-hash.sh
  - PRs #587/#588; docs/verifiable-builds-design.md §4.2
  - pollis-core/src/commands/transparency.rs → BuildVerifyStatus
    (Verified / Pending / Mismatch / Unavailable)
  - docs/reproducible-builds-residuals.md, .github/workflows/rebuild-verify.yml
  - docs/verifiable-builds-design.md §3, desktop-release.yml → provenance job
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


def panel(width, height, color=LINE, fill_opacity=1.0):
    return RoundedRectangle(
        corner_radius=0.1, width=width, height=height,
        stroke_color=color, stroke_width=1.8,
        fill_color=BG, fill_opacity=fill_opacity,
    )


def card(width, height, title, sub, color=LINE, title_color=FG, mono_sub=False):
    box = panel(width, height, color)
    t = Text(title, color=title_color).scale(0.4)
    # Manim's Text has no "default font" sentinel, pass the kwarg or don't.
    sub_kw = {"font": MONO} if mono_sub else {}
    s = Text(sub, color=MUTED, **sub_kw).scale(0.3)
    txt = VGroup(t, s).arrange(DOWN, buff=0.14).move_to(box.get_center())
    g = VGroup(box, txt)
    g.box = box
    g.title = t
    return g


def chip(text, color=FG, w=3.0, h=0.6, scale=0.3):
    b = panel(w, h, color)
    t = Text(text, font=MONO, color=color).scale(scale).move_to(b.get_center())
    g = VGroup(b, t)
    g.box = b
    g.label = t
    return g


class BinaryTransparency(Scene):
    def construct(self):
        self.beat_signing_is_not_enough()
        self.beat_three_layers()
        self.beat_the_bug()
        self.beat_targeted()
        self.beat_reproducibility()
        self.beat_provenance()

    def hold_until(self, target):
        """Pad to `target` seconds so each beat stays readable without audio and
        matches learn/manim/scripts/binary-transparency.md."""
        now = self.renderer.time
        if target > now:
            self.wait(target - now)

    # ── Scene 1 (0:00–0:35), a signature proves less than you think ────────
    def beat_signing_is_not_enough(self):
        head = Text("your OS checks the signature. that proves less than you think.",
                    color=AMBER).scale(0.5).to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        app = card(4.4, 1.6, "Pollis.app", "signed · notarized · valid",
                   color=OK, title_color=FG).move_to([-3.6, 1.1, 0])
        gate = card(3.6, 1.6, "your operating system", "checks the signature",
                    color=LINE).move_to([2.2, 1.1, 0])
        pass_ = Text("allowed to run", color=OK).scale(0.42).move_to([5.6, 1.1, 0])
        self.play(FadeIn(app), FadeIn(gate))
        self.play(app.animate.shift(RIGHT * 1.6), run_time=0.8)
        self.play(FadeIn(pass_))
        self.hold_until(12)

        inside = card(6.6, 1.5, "…but inside these bytes",
                      "a quiet extra process, copying your messages out",
                      color=DANGER, title_color=DANGER).move_to([0, -0.9, 0])
        self.play(FadeIn(inside))
        still = Text("the signature is still perfectly valid.",
                     color=DANGER).scale(0.45).move_to([0, -2.1, 0])
        self.play(FadeIn(still))
        line = Text("a signature proves WHO built it. not WHAT IT DOES.",
                    color=FG).scale(0.5).move_to([0, -2.9, 0])
        self.play(FadeIn(line))
        self.hold_until(24)

        self.play(FadeOut(VGroup(app, gate, pass_, inside, still, line)))
        turn = VGroup(
            Text("so we publish the fingerprint of every release, in public.",
                 color=FG).scale(0.5),
            Text('the question stops being  "did Pollis sign this?"', color=MUTED).scale(0.45),
            Text('and becomes  "is this the app they gave EVERYONE,', color=AMBER).scale(0.5),
            Text('or one made just for ME?"', color=AMBER).scale(0.5),
        ).arrange(DOWN, buff=0.3).move_to([0, 0.2, 0])
        for ln in turn:
            self.play(FadeIn(ln, shift=UP * 0.12), run_time=0.4)
        self.hold_until(35)
        self.play(FadeOut(head), FadeOut(turn))

    # ── Scene 2 (0:35–1:15), the three layers ──────────────────────────────
    def beat_three_layers(self):
        head = Text('three layers, because "the app" isn\'t one file',
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        ledger = panel(12.4, 1.25, LINE).move_to([0, -2.6, 0])
        ledger_t = Text("the public binaries log", color=MUTED).scale(0.34)
        ledger_t.next_to(ledger, UP, buff=0.12)
        self.play(FadeIn(ledger), FadeIn(ledger_t))

        # payload
        p = card(4.0, 1.5, "payload", "before any signature",
                 color=OK, title_color=OK).move_to([-4.3, 1.5, 0])
        p_note = Text("rebuildable from source", color=MUTED).scale(0.32)
        p_note.next_to(p, DOWN, buff=0.2)
        p_chip = chip("payload  a91f…", OK, w=3.4).move_to([-4.3, -2.6, 0])
        self.play(FadeIn(p), FadeIn(p_note))
        self.play(FadeIn(p_chip, shift=DOWN * 0.3))
        self.hold_until(50)

        # signed
        s = card(4.0, 1.5, "signed", "notarized, what you download",
                 color=AMBER, title_color=AMBER).move_to([0, 1.5, 0])
        s_note = Text("never byte-identical twice", color=MUTED).scale(0.32)
        s_note.next_to(s, DOWN, buff=0.2)
        s_chip = chip("signed  7c02…", AMBER, w=3.4).move_to([0, -2.6, 0])
        self.play(FadeIn(s), FadeIn(s_note))
        self.play(FadeIn(s_chip, shift=DOWN * 0.3))
        self.hold_until(65)

        # exe
        e = card(4.0, 1.5, "exe", "the main program, as installed",
                 color=FG, title_color=FG).move_to([4.3, 1.5, 0])
        e_note = Text("what a running app can measure", color=MUTED).scale(0.32)
        e_note.next_to(e, DOWN, buff=0.2)
        e_chip = chip("exe  33be…", FG, w=3.4).move_to([4.3, -2.6, 0])
        self.play(FadeIn(e), FadeIn(e_note))
        self.play(FadeIn(e_chip, shift=DOWN * 0.3))

        bind = Text("all three tied together by the same payload hash",
                    color=MUTED).scale(0.36).move_to([0, -3.45, 0])
        ties = VGroup(
            Line(p_chip.get_right(), s_chip.get_left(), stroke_color=OK, stroke_width=1.5),
            Line(s_chip.get_right(), e_chip.get_left(), stroke_color=OK, stroke_width=1.5),
        )
        self.play(Create(ties), FadeIn(bind))
        why = Text("the third level is what a running app can actually measure.",
                   color=DANGER).scale(0.45).move_to([0, -0.7, 0])
        self.play(FadeIn(why))
        self.hold_until(75)
        self.play(FadeOut(VGroup(head, p, s, e, p_note, s_note, e_note, why,
                                 ledger, ledger_t, p_chip, s_chip, e_chip, ties, bind)))

    # ── Scene 3 (1:15–2:00), the bug, told straight ────────────────────────
    def beat_the_bug(self):
        head = Text("what a running app can measure", color=AMBER).scale(0.55)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        running = card(4.6, 1.3, "your installed app", "asks: am I what they published?",
                       color=FG).move_to([-3.9, 1.5, 0])
        target = card(4.6, 1.3, "compared against", "the payload fingerprint",
                      color=OK, title_color=OK).move_to([3.9, 1.5, 0])
        link = Line(running.box.get_right(), target.box.get_left(),
                    stroke_color=LINE, stroke_width=2)
        self.play(FadeIn(running), Create(link), FadeIn(target))

        whatis = Text("but look at what 'payload' actually is:",
                      color=MUTED).scale(0.42).move_to([0, 0.35, 0])
        opts = VGroup(
            chip("a whole extracted directory tree", MUTED, w=6.0, h=0.6, scale=0.32),
            chip("or an installer file you already ran", MUTED, w=6.0, h=0.6, scale=0.32),
        ).arrange(DOWN, buff=0.25).move_to([0, -0.7, 0])
        self.play(FadeIn(whatis), FadeIn(opts))
        gone = Text("an installed app is neither of those anymore. "
                    "it just has… itself.", color=DANGER).scale(0.44)
        gone.move_to([0, -1.9, 0])
        self.play(FadeIn(gone),
                  *[o.box.animate.set_stroke(DANGER) for o in opts],
                  *[o.label.animate.set_color(DANGER) for o in opts])
        self.hold_until(92)

        alarm = card(9.0, 1.2, "a running app cannot measure a folder or an installer",
                     "it can only measure the program it actually is",
                     color=DANGER, title_color=DANGER).move_to([0, -3.0, 0])
        self.play(FadeIn(alarm), Indicate(alarm, color=DANGER, scale_factor=1.03))
        ours = Text("so the check needs the right thing to compare against.",
                    color=DANGER).scale(0.45).move_to([0, 0.35, 0])
        self.play(FadeOut(whatis), FadeIn(ours))
        self.hold_until(105)

        self.play(FadeOut(VGroup(opts, gone, alarm, ours, target, link)))
        fix = card(6.4, 1.4, "the third level",
                   "the main program's fingerprint, exactly as installed",
                   color=OK, title_color=OK).move_to([3.9, 1.5, 0])
        link2 = Line(running.box.get_right(), fix.box.get_left(),
                     stroke_color=OK, stroke_width=2)
        self.play(FadeIn(fix), Create(link2))
        matched = Text("measure yourself → match", color=OK).scale(0.5).move_to([0, 0.1, 0])
        self.play(FadeIn(matched))

        lessons = VGroup(
            Text("a running app can only measure the program it is.",
                 color=FG).scale(0.44),
            Text("and releases from before that layer have nothing to compare against,",
                 color=MUTED).scale(0.4),
            Text('so the app says "can\'t check", not "you\'ve been tampered with".',
                 color=AMBER).scale(0.46),
        ).arrange(DOWN, buff=0.28).move_to([0, -1.9, 0])
        for ln in lessons:
            self.play(FadeIn(ln, shift=UP * 0.12), run_time=0.4)
        self.hold_until(120)
        self.play(FadeOut(VGroup(head, running, fix, link2, matched, lessons)))

    # ── Scene 4 (2:00–2:40), the targeted backdoor ─────────────────────────
    def beat_targeted(self):
        head = Text("so what does this actually buy you?", color=AMBER).scale(0.55)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        pub = card(5.4, 1.4, "user A downloads the public build",
                   "fingerprint is in the log, match", color=OK, title_color=OK)
        pub.move_to([-3.4, 1.6, 0])
        tgt = card(5.4, 1.4, "user B is handed a special build",
                   "built for exactly one person", color=DANGER, title_color=DANGER)
        tgt.move_to([3.4, 1.6, 0])
        self.play(FadeIn(pub), FadeIn(tgt))
        self.hold_until(140)

        a = card(5.4, 1.7, "(a) it isn't in the log",
                 "user B's own app notices immediately", color=OK, title_color=OK)
        a.move_to([-3.4, -0.7, 0])
        b = card(5.4, 1.7, "(b) we DO log it",
                 "then it sits in public, forever, beside the real release",
                 color=OK, title_color=OK)
        b.move_to([3.4, -0.7, 0])
        arrows = VGroup(
            Line(tgt.box.get_bottom(), a.box.get_top(), stroke_color=LINE, stroke_width=1.5),
            Line(tgt.box.get_bottom(), b.box.get_top(), stroke_color=LINE, stroke_width=1.5),
        )
        self.play(Create(arrows), FadeIn(a), FadeIn(b))
        no_quiet = Text("there is no quiet option.", color=AMBER).scale(0.6)
        no_quiet.move_to([0, -2.6, 0])
        self.play(FadeIn(no_quiet))
        self.hold_until(160)
        self.play(FadeOut(VGroup(head, pub, tgt, a, b, arrows, no_quiet)))

    # ── Scene 5 (2:40–3:22), reproducibility, honestly ─────────────────────
    def beat_reproducibility(self):
        head = Text("now the part to be careful about", color=AMBER).scale(0.6)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))
        gap = VGroup(
            Text("a fingerprint proves we published these bytes.", color=FG).scale(0.48),
            Text("it does NOT prove they came from the source code you can read.",
                 color=DANGER).scale(0.48),
            Text("closing that gap = reproducible builds: compile the source, "
                 "get byte-identical output.", color=MUTED).scale(0.4),
        ).arrange(DOWN, buff=0.26).move_to([0, 1.9, 0])
        for ln in gap:
            self.play(FadeIn(ln, shift=UP * 0.12), run_time=0.4)
        self.hold_until(175)

        rows = VGroup(
            card(11.0, 1.0, "Linux AppImage, REPRODUCIBLE",
                 "an independent rebuilder, holding none of our secrets, gets the "
                 "hash we logged", color=OK, title_color=OK),
            card(11.0, 1.0, "macOS and Windows, NOT YET",
                 "best-effort. the largest open gap in this story, and we won't "
                 "imply otherwise", color=AMBER, title_color=AMBER),
            card(11.0, 1.0, "the signing layer, NEVER REPRODUCIBLE",
                 "by construction, on every platform. by design, not neglect",
                 color=MUTED, title_color=MUTED),
        ).arrange(DOWN, buff=0.3).move_to([0, -1.2, 0])
        for r in rows:
            self.play(FadeIn(r, shift=UP * 0.1), run_time=0.5)
            self.wait(0.3)
        self.hold_until(202)
        self.play(FadeOut(VGroup(head, gap, rows)))

    # ── Scene 6 (3:22–3:40), the second, independent leg ───────────────────
    def beat_provenance(self):
        head = Text("one more leg", color=AMBER).scale(0.6).move_to([0, 2.7, 0])
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        left = card(5.6, 2.0, "reproducibility", "does it match the source?",
                    color=OK, title_color=OK).move_to([-3.4, 0.9, 0])
        right = card(5.6, 2.0, "provenance (SLSA + cosign)", "which workflow built it?",
                     color=OK, title_color=OK).move_to([3.4, 0.9, 0])
        self.play(FadeIn(left), FadeIn(right))

        log = card(9.0, 1.3, "recorded in a public log that isn't ours",
                   "no Pollis-held key anywhere on the verification path",
                   color=FG).move_to([0, -1.3, 0])
        stems = VGroup(
            Line(left.box.get_bottom(), log.box.get_top(), stroke_color=LINE, stroke_width=1.5),
            Line(right.box.get_bottom(), log.box.get_top(), stroke_color=LINE, stroke_width=1.5),
        )
        self.play(Create(stems), FadeIn(log))
        close = Text("different questions. two independent legs under the same claim.",
                     color=AMBER).scale(0.46).move_to([0, -2.7, 0])
        self.play(FadeIn(close))
        self.hold_until(220)
        self.play(FadeOut(VGroup(head, left, right, log, stems, close)))
        self.wait(0.5)
