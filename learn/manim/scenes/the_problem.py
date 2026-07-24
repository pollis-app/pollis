"""
Intro topic, "The problem with most messaging apps".

One continuous ~1:45 scene, four beats, NO audio (added later). Establishes why
E2EE matters at all: an ordinary app keeps a readable copy of your message, and
that copy is a liability. Sets up the rest of the section. Sentence case, neutral
framing (the point is what Pollis does, not a confession).

Render (see learn/manim/render.sh):
    learn/manim/render.sh TheProblem the-problem m
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


def panel(width, height, color=LINE, fill_opacity=1.0, fill=BG):
    return RoundedRectangle(
        corner_radius=0.1, width=width, height=height,
        stroke_color=color, stroke_width=1.8,
        fill_color=fill, fill_opacity=fill_opacity,
    )


def node(x, y, title, sub, color=LINE, w=3.2, h=1.3, title_color=FG):
    box = panel(w, h, color).move_to([x, y, 0])
    t = Text(title, color=title_color).scale(0.4)
    s = Text(sub, color=MUTED).scale(0.28)
    txt = VGroup(t, s).arrange(DOWN, buff=0.12).move_to(box.get_center())
    g = VGroup(box, txt)
    g.box = box
    return g


def note(text, color=FG, w=6.4, h=0.8, scale=0.34):
    b = panel(w, h, color)
    t = Text(text, color=color).scale(scale).move_to(b.get_center())
    g = VGroup(b, t)
    g.box = b
    g.label = t
    return g


class TheProblem(Scene):
    def construct(self):
        self.beat_how_it_works()
        self.beat_the_copy()
        self.beat_risks()
        self.beat_fix()

    def hold_until(self, target):
        now = self.renderer.time
        if target > now:
            self.wait(target - now)

    # ── Scene 1 (0:00–0:22) — how most apps work ────────────────────────────
    def beat_how_it_works(self):
        head = Text("How most messaging apps work", color=AMBER).scale(0.6)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.head1 = head
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        self.you = node(-4.8, 0.8, "You", "your phone")
        self.server = node(0, 0.8, "The company", "servers")
        self.them = node(4.8, 0.8, "Your friend", "their phone")
        self.play(FadeIn(self.you), FadeIn(self.server), FadeIn(self.them))
        self.wires = VGroup(
            Line(self.you.box.get_right(), self.server.box.get_left(),
                 stroke_color=LINE, stroke_width=2),
            Line(self.server.box.get_right(), self.them.box.get_left(),
                 stroke_color=LINE, stroke_width=2),
        )
        self.play(Create(self.wires))

        msg = note('"Meet me at seven"', FG, w=3.6, h=0.7, scale=0.34)
        msg.move_to(self.you.box.get_center() + DOWN * 1.6)
        self.play(FadeIn(msg))
        self.play(msg.animate.move_to(self.server.box.get_center() + DOWN * 1.6), run_time=1.0)
        readable = Text("Stored here, readable", color=DANGER).scale(0.42)
        readable.move_to([0, -1.5, 0])
        self.play(FadeIn(readable), Indicate(self.server, color=DANGER, scale_factor=1.04))
        self.play(msg.animate.move_to(self.them.box.get_center() + DOWN * 1.6), run_time=1.0)
        cap = Text("The company keeps a readable copy of what you wrote.",
                   color=FG).scale(0.46).move_to([0, -2.7, 0])
        self.play(FadeIn(cap))
        self.hold_until(22)
        self.play(FadeOut(VGroup(msg, readable, cap)))

    # ── Scene 2 (0:22–0:45) — that copy is a liability ──────────────────────
    def beat_the_copy(self):
        useful = note("That copy powers search, backups, and syncing",
                      MUTED, w=8.6, h=0.8, scale=0.34).move_to([0, -1.4, 0])
        self.play(FadeIn(useful))
        but = Text("But your message is only as private as a promise not to look,",
                   color=FG).scale(0.44).move_to([0, -2.5, 0])
        but2 = Text("and the security of everything that promise depends on.",
                    color=FG).scale(0.44).move_to([0, -3.1, 0])
        self.play(FadeIn(but))
        self.play(FadeIn(but2))
        self.hold_until(45)
        self.play(FadeOut(VGroup(useful, but, but2, self.head1, self.you,
                                 self.server, self.them, self.wires)))

    # ── Scene 3 (0:45–1:15) — the three risks ───────────────────────────────
    def beat_risks(self):
        head = Text("What a readable copy can become", color=AMBER).scale(0.55)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        rows = VGroup(
            node(0, 1.7, "Read to target ads or train models",
                 "what you say and who you say it to is valuable",
                 color=DANGER, w=10.6, h=1.1, title_color=DANGER),
            node(0, 0.2, "Handed over on demand",
                 "to a government or the law, sometimes for no crime at all",
                 color=DANGER, w=10.6, h=1.1, title_color=DANGER),
            node(0, -1.3, "Stolen in a breach",
                 "a central readable copy is a target; hack the company, read it all",
                 color=DANGER, w=10.6, h=1.1, title_color=DANGER),
        )
        for r in rows:
            self.play(FadeIn(r, shift=UP * 0.12), run_time=0.5)
        none_evil = Text("None of this needs the company to be evil, only breakable.",
                         color=FG).scale(0.46).move_to([0, -2.9, 0])
        self.play(FadeIn(none_evil))
        self.hold_until(75)
        self.play(FadeOut(VGroup(head, rows, none_evil)))

    # ── Scene 4 (1:15–1:45) — the fix ───────────────────────────────────────
    def beat_fix(self):
        head = Text("The fix: make the copy unreadable", color=AMBER).scale(0.55)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        you = node(-4.8, 1.0, "You", "seal it here", color=OK, title_color=OK)
        server = node(0, 1.0, "The company", "carries noise it can't open", color=MUTED)
        them = node(4.8, 1.0, "Your friend", "opens it here", color=OK, title_color=OK)
        wires = VGroup(
            Line(you.box.get_right(), server.box.get_left(), stroke_color=LINE, stroke_width=2),
            Line(server.box.get_right(), them.box.get_left(), stroke_color=LINE, stroke_width=2),
        )
        self.play(FadeIn(you), FadeIn(server), FadeIn(them), Create(wires))

        sealed = note("▓▓▓▓▓▓▓▓▓▓▓▓", OK, w=3.6, h=0.7, scale=0.34)
        sealed.move_to(you.box.get_center() + DOWN * 1.6)
        self.play(FadeIn(sealed))
        self.play(sealed.animate.move_to(server.box.get_center() + DOWN * 1.6), run_time=1.0)
        cant = Text("The company cannot read it, even if asked or forced.",
                    color=OK).scale(0.46).move_to([0, -1.5, 0])
        self.play(FadeIn(cant))
        self.play(sealed.animate.move_to(them.box.get_center() + DOWN * 1.6), run_time=1.0)
        close = Text("That is what Pollis does. The rest of this section explains how.",
                     color=AMBER).scale(0.48).move_to([0, -2.9, 0])
        self.play(FadeIn(close))
        self.hold_until(100)
        self.play(FadeOut(VGroup(head, you, server, them, wires, sealed, cant, close)))
        self.wait(0.4)
