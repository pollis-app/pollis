"""
Topic 5 — "Forward secrecy and post-compromise security" (#594).

One continuous ~2:45 scene, six beats, NO audio (added later). Time runs left to
right in every beat so both properties are literally directional. The page and
this scene never say or imply "messages delete themselves" — the claim is about
what a stolen key opens, backwards and forwards.

Render (see learn/manim/render.sh):
    learn/manim/render.sh WindowNotDoor window-not-door m

Accuracy anchors:
  - OpenMLS / RFC 9420 §8 key schedule; pollis-core/src/commands/mls.rs
  - .codesight/wiki/mls.md → "Reconcile Flow" (epoch advances on every commit)
  - CLAUDE.md → "Storage" (local ciphertext + MLS state — the "screen" caveat)
  - docs/security-whitepaper.md + Topic 1 (metadata is untouched by either
    property)
"""

from manim import (
    DOWN,
    LEFT,
    RIGHT,
    UP,
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


def panel(width, height, color=LINE, fill_opacity=1.0, fill=BG):
    return RoundedRectangle(
        corner_radius=0.1, width=width, height=height,
        stroke_color=color, stroke_width=1.8,
        fill_color=fill, fill_opacity=fill_opacity,
    )


def chip(text, color=FG, w=2.0, h=0.6, scale=0.3, mono=True):
    b = panel(w, h, color)
    kw = {"font": MONO} if mono else {}
    t = Text(text, color=color, **kw).scale(scale).move_to(b.get_center())
    g = VGroup(b, t)
    g.box = b
    g.label = t
    return g


class WindowNotDoor(Scene):
    def construct(self):
        self.beat_setup()
        self.beat_forward_secrecy()
        self.beat_caveat()
        self.beat_pcs()
        self.beat_window()
        self.beat_limits()

    def hold_until(self, target):
        """Pad to `target` seconds so each beat stays readable without audio and
        matches learn/manim/scripts/window-not-door.md."""
        now = self.renderer.time
        if target > now:
            self.wait(target - now)

    def timeline(self, y=0.4):
        """The shared spine: five sealed epochs, time running left to right."""
        axis = Line([-6.4, y, 0], [6.4, y, 0], stroke_color=LINE, stroke_width=2)
        marks = VGroup(*[
            chip(f"epoch {n}", MUTED, w=2.1, h=0.6).move_to([-5.2 + i * 2.6, y, 0])
            for i, n in enumerate((5, 6, 7, 8, 9))
        ])
        return axis, marks

    # ── Scene 1 (0:00–0:20) — assume the worst ──────────────────────────────
    def beat_setup(self):
        head = Text("assume the worst: someone has a key off your device, today",
                    color=AMBER).scale(0.52).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        self.axis, self.marks = self.timeline()
        self.play(Create(self.axis), FadeIn(self.marks))

        stolen = chip("stolen key", DANGER, w=2.6, h=0.7, scale=0.32)
        stolen.move_to([0, 1.8, 0])
        arrow = Line(stolen.box.get_bottom(), self.marks[2].box.get_top(),
                     stroke_color=DANGER, stroke_width=2)
        self.play(FadeIn(stolen), Create(arrow))
        self.stolen, self.stolen_arrow = stolen, arrow

        q1 = Text("←  what about the past?", color=FG).scale(0.5).move_to([-3.6, -1.2, 0])
        q2 = Text("what about the future?  →", color=FG).scale(0.5).move_to([3.6, -1.2, 0])
        self.play(FadeIn(q1), FadeIn(q2))
        self.hold_until(20)
        self.play(FadeOut(q1), FadeOut(q2), FadeOut(head))

    # ── Scene 2 (0:20–0:55) — forward secrecy ───────────────────────────────
    def beat_forward_secrecy(self):
        head = Text("the past: forward secrecy", color=AMBER).scale(0.6)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        for i in (1, 0):
            self.play(Indicate(self.marks[i], color=DANGER, scale_factor=1.1), run_time=0.4)
        nothing = Text("they try yesterday's messages — and get nothing.",
                       color=DANGER).scale(0.48).move_to([0, -1.1, 0])
        self.play(FadeIn(nothing),
                  *[self.marks[i].box.animate.set_stroke(OK) for i in (0, 1)],
                  *[self.marks[i].label.animate.set_color(OK) for i in (0, 1)])
        self.hold_until(32)

        self.play(FadeOut(nothing))
        chain = VGroup(*[chip(f"key {n}", FG, w=1.9, h=0.6) for n in (5, 6, 7)])
        chain.arrange(RIGHT, buff=1.5).move_to([0, -1.6, 0])
        arrows = VGroup(*[
            Line(chain[i].box.get_right(), chain[i + 1].box.get_left(),
                 stroke_color=OK, stroke_width=2) for i in range(2)
        ])
        oneway = Text("one-way step: easy forwards, impossible backwards "
                      "(mixing paint, not un-mixing it)", color=MUTED).scale(0.36)
        oneway.move_to([0, -2.5, 0])
        self.play(FadeIn(chain), Create(arrows), FadeIn(oneway))

        # Try to run one backwards.
        back = Line(chain[1].box.get_left(), chain[0].box.get_right(),
                    stroke_color=DANGER, stroke_width=3)
        self.play(Create(back), run_time=0.5)
        refused = Text("×  cannot be run backwards", color=DANGER).scale(0.42)
        refused.move_to([0, -3.2, 0])
        self.play(FadeIn(refused))
        self.play(*[c.box.animate.set_stroke(MUTED).set_opacity(0.35) for c in chain[:2]],
                  *[c.label.animate.set_opacity(0.35) for c in chain[:2]])
        deleted = Text("and once a key is used, it is deleted — "
                       "not on your device, not on our servers, nowhere.",
                       color=FG).scale(0.42).move_to([0, -3.8, 0])
        self.play(FadeIn(deleted))
        self.hold_until(55)
        self.play(FadeOut(VGroup(head, chain, arrows, back, refused, oneway, deleted)))

    # ── Scene 3 (0:55–1:20) — the caveat that gets skipped ──────────────────
    def beat_caveat(self):
        head = Text("the honest caveat — do not skip this one",
                    color=DANGER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        spine = VGroup(self.axis, self.marks, self.stolen, self.stolen_arrow)
        self.play(Write(head), spine.animate.set_opacity(0.0))

        screen = panel(7.0, 2.6, DANGER).move_to([0, -1.4, 0])
        lines = VGroup(
            Text('boris:  "anyone around?"', font=MONO, color=FG).scale(0.34),
            Text('you:    "on my way"', font=MONO, color=FG).scale(0.34),
            Text('boris:  "see you at 7"', font=MONO, color=FG).scale(0.34),
        ).arrange(DOWN, buff=0.22, aligned_edge=LEFT).move_to(screen.get_center())
        cap = Text("your unlocked device, still showing yesterday in plaintext",
                   color=MUTED).scale(0.34).next_to(screen, UP, buff=0.2)
        self.play(FadeIn(screen), FadeIn(lines), FadeIn(cap))
        reads = Text("someone holding the device just reads them. "
                     "that is not a cryptography question.",
                     color=DANGER).scale(0.44).move_to([0, -3.2, 0])
        self.play(FadeIn(reads))
        real = Text("what forward secrecy DOES promise: traffic recorded months ago "
                    "stays sealed, even with today's key.", color=OK).scale(0.4)
        real.move_to([0, 1.7, 0])
        self.play(FadeIn(real))
        self.hold_until(80)
        self.play(FadeOut(VGroup(head, screen, lines, cap, reads, real)),
                  spine.animate.set_opacity(1.0))

    # ── Scene 4 (1:20–2:00) — post-compromise security ──────────────────────
    def beat_pcs(self):
        head = Text("the future: post-compromise security",
                    color=AMBER).scale(0.6).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        reading = Text("the attacker is reading. this is a real break.",
                       color=DANGER).scale(0.46).move_to([0, -1.1, 0])
        self.play(FadeIn(reading))
        for i in (2, 3):
            self.play(self.marks[i].box.animate.set_stroke(DANGER),
                      self.marks[i].label.animate.set_color(DANGER), run_time=0.35)
            self.play(Indicate(self.marks[i], color=DANGER, scale_factor=1.1), run_time=0.35)
        self.hold_until(95)

        ordinary = chip("someone else does something ordinary: a routine key update",
                        OK, w=9.6, h=0.8, scale=0.34).move_to([0, -2.2, 0])
        self.play(FadeOut(reading), FadeIn(ordinary))
        flow = Text("new keys flow up the path…", color=OK).scale(0.42)
        flow.move_to([0, -1.1, 0])
        self.play(FadeIn(flow))
        self.play(self.marks[4].box.animate.set_stroke(OK),
                  self.marks[4].label.animate.set_color(OK))
        self.play(Indicate(self.marks[4], color=OK, scale_factor=1.15), run_time=0.6)

        out = Text("epoch 9 arrives — and the attacker is out.",
                   color=OK).scale(0.5).move_to([0, -1.1, 0])
        self.play(FadeOut(flow), FadeIn(out),
                  self.stolen.box.animate.set_stroke(MUTED).set_opacity(0.35),
                  self.stolen.label.animate.set_opacity(0.35),
                  self.stolen_arrow.animate.set_stroke(MUTED).set_opacity(0.3))
        nobody = Text("nobody detected anything. nobody ran an incident response.",
                      color=FG).scale(0.44).move_to([0, -3.0, 0])
        self.play(FadeIn(nobody))
        healed = Text("the group healed by carrying on normally.",
                      color=AMBER).scale(0.48).move_to([0, -3.6, 0])
        self.play(FadeIn(healed))
        self.hold_until(120)
        self.play(FadeOut(VGroup(head, ordinary, out, nobody, healed)))

    # ── Scene 5 (2:00–2:25) — the window ────────────────────────────────────
    def beat_window(self):
        head = Text("a stolen key is a window, not a door",
                    color=AMBER).scale(0.6).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        band = panel(5.2, 1.5, DANGER, fill_opacity=0.12, fill=DANGER)
        band.move_to([0, 0.4, 0])
        left_wall = Text("← sealed by forward secrecy", color=OK).scale(0.38)
        left_wall.move_to([-4.4, -0.9, 0])
        right_wall = Text("sealed by the next commit →", color=OK).scale(0.38)
        right_wall.move_to([4.4, -0.9, 0])
        self.play(FadeIn(band), FadeIn(left_wall), FadeIn(right_wall))
        self.hold_until(133)

        narrow = panel(2.2, 1.5, DANGER, fill_opacity=0.12, fill=DANGER)
        narrow.move_to([0, 0.4, 0])
        self.play(band.animate.become(narrow), run_time=1.0)
        more = Text("the more the group talks, the narrower it gets — "
                    "which is what cheap rekeying bought us.",
                    color=FG).scale(0.44).move_to([0, -2.0, 0])
        self.play(FadeIn(more))
        self.hold_until(145)
        self.play(FadeOut(VGroup(head, band, left_wall, right_wall, more,
                                 self.axis, self.marks, self.stolen, self.stolen_arrow)))

    # ── Scene 6 (2:25–2:50) — the limits ────────────────────────────────────
    def beat_limits(self):
        head = Text("the limits, because this gets oversold",
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        rows = VGroup(
            chip("continuous access to your device → they get every update too",
                 DANGER, w=11.4, h=0.8, scale=0.34),
            chip("already-decrypted messages on your device → readable by whoever holds it",
                 DANGER, w=11.4, h=0.8, scale=0.34),
            chip("metadata → untouched by either property (topic 1)",
                 AMBER, w=11.4, h=0.8, scale=0.34),
            chip("healing is driven by activity → a silent group rekeys rarely",
                 AMBER, w=11.4, h=0.8, scale=0.34),
        ).arrange(DOWN, buff=0.35).move_to([0, 0.7, 0])
        for r in rows:
            self.play(FadeIn(r, shift=UP * 0.1), run_time=0.4)

        close = VGroup(
            Text("PCS recovers from a break that ENDED.", color=OK).scale(0.5),
            Text("it does not fix one that is still happening.", color=DANGER).scale(0.5),
        ).arrange(DOWN, buff=0.24).move_to([0, -2.4, 0])
        self.play(FadeIn(close))
        self.hold_until(168)
        self.play(FadeOut(VGroup(head, rows, close)))
        self.wait(0.5)
