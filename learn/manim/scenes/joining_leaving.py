"""
Topic 4 — "Joining, leaving, multi-device, and the honest history boundary" (#593).

One continuous ~2:50 scene, six beats, NO audio (added later). The load-bearing
beat is the history boundary: pre-join messages are shown TRYING to open and
visibly failing, because "not hidden — unavailable" is the point. The final beat
draws the backup trade as a fork where neither path is free.

Render (see learn/manim/render.sh):
    learn/manim/render.sh JoiningLeaving joining-leaving m

Accuracy anchors:
  - .codesight/wiki/mls.md → "Multi-Device Enrollment", "GroupInfo Publishing"
  - CLAUDE.md → "Messages must work; history is bounded, not flaky" (the two
    acceptable losses, verbatim policy; no Megolm-style backup)
  - CLAUDE.md → "Storage" (ciphertext + MLS state live locally)
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


def chip(text, color=FG, w=2.4, h=0.6, scale=0.3, mono=True):
    b = panel(w, h, color)
    kw = {"font": MONO} if mono else {}
    t = Text(text, color=color, **kw).scale(scale).move_to(b.get_center())
    g = VGroup(b, t)
    g.box = b
    g.label = t
    return g


def card(w, h, title, sub, color=LINE, title_color=FG):
    b = panel(w, h, color)
    t = Text(title, color=title_color).scale(0.38)
    s = Text(sub, color=MUTED).scale(0.3)
    g = VGroup(b, VGroup(t, s).arrange(DOWN, buff=0.14).move_to(b.get_center()))
    g.box = b
    return g


class JoiningLeaving(Scene):
    def construct(self):
        self.beat_key_packages()
        self.beat_welcome()
        self.beat_boundary()
        self.beat_removal()
        self.beat_multi_device()
        self.beat_trade()

    def hold_until(self, target):
        """Pad to `target` seconds so each beat stays readable without audio and
        matches learn/manim/scripts/joining-leaving.md."""
        now = self.renderer.time
        if target > now:
            self.wait(target - now)

    # ── Scene 1 (0:00–0:25) — key packages ──────────────────────────────────
    def beat_key_packages(self):
        head = Text("every device leaves a lockbox at the front desk, in advance",
                    color=AMBER).scale(0.52).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        shelf = panel(11.0, 1.6, MUTED).move_to([0, -1.2, 0])
        shelf_t = Text("the server — holds them, cannot open them",
                       color=MUTED).scale(0.34).next_to(shelf, DOWN, buff=0.2)
        self.play(FadeIn(shelf), FadeIn(shelf_t))

        devices = VGroup(*[chip(n, FG, w=2.4) for n in
                           ("ariel's phone", "boris's laptop", "chen's phone")])
        devices.arrange(RIGHT, buff=0.7).move_to([0, 1.7, 0])
        self.play(FadeIn(devices))

        boxes = VGroup(*[chip("key package", OK, w=2.8, h=0.7, scale=0.3)
                         for _ in range(3)])
        for i, b in enumerate(boxes):
            b.move_to(devices[i].get_center())
        self.play(*[b.animate.move_to([-3.5 + i * 3.5, -1.2, 0]) for i, b in enumerate(boxes)],
                  run_time=1.2)
        note = Text("anyone can put something IN. only that device can open it.",
                    color=FG).scale(0.44).move_to([0, -2.7, 0])
        self.play(FadeIn(note))
        self.hold_until(25)
        self.play(FadeOut(VGroup(head, devices, note)))
        self.shelf, self.shelf_t, self.boxes = shelf, shelf_t, boxes

    # ── Scene 2 (0:25–0:55) — add + welcome ─────────────────────────────────
    def beat_welcome(self):
        head = Text("adding you: a Welcome, sealed to your lockbox",
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        member = chip("an existing member", FG, w=4.2, h=0.8, scale=0.32)
        member.move_to([-4.2, 1.6, 0])
        self.play(FadeIn(member))
        picked = self.boxes[1]
        self.play(Indicate(picked, color=AMBER, scale_factor=1.15))
        self.play(picked.animate.move_to([-4.2, 0.5, 0]))

        welcome = chip("Welcome: the group's state, as of NOW", OK, w=7.4, h=0.8, scale=0.34)
        welcome.move_to([1.2, 0.5, 0])
        self.play(FadeIn(welcome, shift=RIGHT * 0.3))
        joined = chip("you're in — holding this epoch's keys", OK, w=7.0, h=0.8, scale=0.34)
        joined.move_to([1.2, -2.9, 0])
        self.play(welcome.animate.move_to([1.2, -2.9, 0]), run_time=0.9)
        self.play(FadeOut(welcome), FadeIn(joined))
        self.hold_until(55)
        self.play(FadeOut(VGroup(head, member, picked, joined, self.shelf,
                                 self.shelf_t, self.boxes[0], self.boxes[2])))

    # ── Scene 3 (0:55–1:30) — the history boundary ──────────────────────────
    def beat_boundary(self):
        head = Text("the part people don't expect", color=AMBER).scale(0.6)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        line = Line([0, 2.2, 0], [0, -2.2, 0], stroke_color=AMBER, stroke_width=3)
        line_lab = Text("the moment you joined", color=AMBER).scale(0.34)
        line_lab.next_to(line, DOWN, buff=0.2)

        before = VGroup(*[chip("▓▓▓▓▓▓▓▓", MUTED, w=2.4, h=0.55, scale=0.3)
                          for _ in range(4)]).arrange(DOWN, buff=0.28)
        before.move_to([-3.6, 0.2, 0])
        after = VGroup(*[chip(t, OK, w=3.0, h=0.55, scale=0.3) for t in
                         ('"anyone around?"', '"on my way"', '"see you at 7"')])
        after.arrange(DOWN, buff=0.28).move_to([3.6, 0.2, 0])

        self.play(FadeIn(before), Create(line), FadeIn(line_lab), FadeIn(after))
        ok_lab = Text("opens fine", color=OK).scale(0.36).next_to(after, UP, buff=0.25)
        self.play(FadeIn(ok_lab))
        self.hold_until(70)

        # Try to open the old ones — and visibly fail.
        trying = Text("trying your key on the older ones…", color=FG).scale(0.42)
        trying.move_to([0, -2.9, 0])
        self.play(FadeIn(trying))
        for b in before:
            self.play(Indicate(b, color=DANGER, scale_factor=1.08), run_time=0.3)
        self.play(*[b.box.animate.set_stroke(DANGER) for b in before],
                  *[b.label.animate.set_color(DANGER) for b in before])
        failed = Text("the key genuinely does not fit. not hidden — UNAVAILABLE.",
                      color=DANGER).scale(0.48).move_to([0, -2.9, 0])
        self.play(FadeOut(trying), FadeIn(failed))
        why = Text("new keys can't be used to work out old ones — "
                   "that's what makes removal real (topic 5)",
                   color=MUTED).scale(0.38).move_to([0, -3.5, 0])
        self.play(FadeIn(why))
        self.hold_until(90)
        self.play(FadeOut(VGroup(head, before, after, line, line_lab, ok_lab, failed, why)))

    # ── Scene 4 (1:30–1:55) — removal, mirroring topic 3 ────────────────────
    def beat_removal(self):
        head = Text("leaving is the same idea, pointed the other way",
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        who = chip("the removed device", DANGER, w=4.6, h=0.8, scale=0.34)
        who.move_to([-4.0, 1.2, 0])
        keys = chip("keys along their path: replaced", OK, w=6.4, h=0.8, scale=0.34)
        keys.move_to([2.4, 1.2, 0])
        self.play(FadeIn(who), FadeIn(keys))

        kept = chip("what they already decrypted stays decrypted", MUTED, w=8.4,
                    h=0.8, scale=0.34).move_to([0, -0.1, 0])
        self.play(FadeIn(kept))
        honest = Text("(we can't reach into their machine, and we won't claim we can)",
                      color=MUTED).scale(0.36).move_to([0, -0.9, 0])
        self.play(FadeIn(honest))

        nxt = chip("everything from this epoch forward ▓▓▓▓▓▓▓▓", DANGER, w=8.4,
                   h=0.8, scale=0.34).move_to([0, -2.1, 0])
        self.play(FadeIn(nxt))
        arith = Text("removal is arithmetic — not a permission flag on a server "
                     "you're asked to trust.", color=OK).scale(0.44)
        arith.move_to([0, -3.1, 0])
        self.play(FadeIn(arith))
        self.hold_until(115)
        self.play(FadeOut(VGroup(head, who, keys, kept, honest, nxt, arith)))

    # ── Scene 5 (1:55–2:20) — multi-device is the same mechanism ────────────
    def beat_multi_device(self):
        head = Text("your own devices are separate members",
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        phone = card(4.6, 1.4, "your phone", "its own keys, its own leaf",
                     color=OK, title_color=OK).move_to([-3.4, 1.5, 0])
        laptop = card(4.6, 1.4, "your laptop", "its own keys, its own leaf",
                      color=OK, title_color=OK).move_to([3.4, 1.5, 0])
        self.play(FadeIn(phone), FadeIn(laptop))
        nolink = Text("nothing is copied between them — a key that travels is a key "
                      "that can be stolen on the way", color=MUTED).scale(0.38)
        nolink.move_to([0, 0.35, 0])
        self.play(FadeIn(nolink))
        self.hold_until(132)

        newlap = card(9.0, 1.4, "a NEW laptop, added today",
                      "an add-commit, exactly like adding a person",
                      color=AMBER, title_color=AMBER).move_to([0, -1.1, 0])
        self.play(FadeIn(newlap))
        empty = Text("its history before today: empty. same boundary, same reason.",
                     color=DANGER).scale(0.46).move_to([0, -2.5, 0])
        self.play(FadeIn(empty))
        self.hold_until(140)
        self.play(FadeOut(VGroup(head, phone, laptop, nolink, newlap, empty)))

    # ── Scene 6 (2:20–2:55) — the trade, as a fork ──────────────────────────
    def beat_trade(self):
        head = Text("the trade — hear it from us, not later",
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        a = panel(6.2, 3.0, DANGER).move_to([-3.5, 0.5, 0])
        a_t = VGroup(
            Text("path A: back up history", color=DANGER).scale(0.4),
            Text("encrypt it, store it on our servers,", color=MUTED).scale(0.32),
            Text("protect it with a 6-digit PIN", color=MUTED).scale(0.32),
            Text("= a copy of every conversation,", color=DANGER).scale(0.34),
            Text("on our infrastructure, behind", color=DANGER).scale(0.34),
            Text("a guessable number", color=DANGER).scale(0.34),
        ).arrange(DOWN, buff=0.16).move_to(a.get_center())

        b = panel(6.2, 3.0, OK).move_to([3.5, 0.5, 0])
        b_t = VGroup(
            Text("path B: no backup", color=OK).scale(0.4),
            Text("the server holds nothing", color=MUTED).scale(0.32),
            Text("a new device starts empty", color=MUTED).scale(0.32),
            Text("= your old messages live on", color=OK).scale(0.34),
            Text("your old device, and nowhere", color=OK).scale(0.34),
            Text("else. that cost is real.", color=OK).scale(0.34),
        ).arrange(DOWN, buff=0.16).move_to(b.get_center())

        self.play(FadeIn(a), FadeIn(a_t))
        self.play(FadeIn(b), FadeIn(b_t))
        pick = Text("we take B. neither path is free, and we're not going to dress it up.",
                    color=AMBER).scale(0.46).move_to([0, -1.6, 0])
        self.play(FadeIn(pick))
        self.hold_until(160)

        promise = VGroup(
            Text("exactly two kinds of loss are acceptable:", color=FG).scale(0.44),
            Text("messages sent before you joined · a new device starting empty",
                 color=MUTED).scale(0.4),
            Text("everything else must work. anything else missing is a BUG.",
                 color=OK).scale(0.46),
        ).arrange(DOWN, buff=0.22).move_to([0, -2.8, 0])
        self.play(FadeIn(promise))
        self.hold_until(175)
        self.play(FadeOut(VGroup(head, a, a_t, b, b_t, pick, promise)))
        self.wait(0.5)
