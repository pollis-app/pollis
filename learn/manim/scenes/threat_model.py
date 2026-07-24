"""
Topic 1, "Threat model: who can see what" (#590).

One continuous ~2:10 scene, five beats, NO audio (added later). Built as ONE
diagram that accumulates rather than five unrelated scenes, so the reader leaves
holding a single image they can return to.

Colour discipline established here and reused across all twelve topics:
    green  = sealed, we cannot see it
    amber  = visible to us, and we say so (metadata)
    red    = outside what any messenger defends (your device, our bugs)

Render (see learn/manim/render.sh):
    learn/manim/render.sh ThreatModel threat-model m

Accuracy anchors:
  - docs/security-whitepaper.md §1.1 (trust delegation, honest limits)
  - .codesight/wiki/safety.md → "Threat model"
  - CLAUDE.md → "Security model" (trusted: device, local DB, signed binary, OS
    keystore; untrusted: network, Turso, DS, operators)
  - docs/verifiable-builds-design.md §0 (signing alone ≠ binary honesty)
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


def envelope(color, label, w=2.4, h=0.8, scale=0.32):
    b = panel(w, h, color)
    t = Text(label, font=MONO, color=color).scale(scale).move_to(b.get_center())
    g = VGroup(b, t)
    g.box = b
    g.label = t
    return g


class ThreatModel(Scene):
    def construct(self):
        self.beat_most_apps()
        self.beat_sealed()
        self.beat_metadata()
        self.beat_key_substitution()
        self.beat_boundary()

    def hold_until(self, target):
        """Pad to `target` seconds so each beat stays readable without audio and
        matches learn/manim/scripts/threat-model.md."""
        now = self.renderer.time
        if target > now:
            self.wait(target - now)

    # ── Scene 1 (0:00–0:20), how most apps work ────────────────────────────
    def beat_most_apps(self):
        head = Text("most apps ask you to trust a company's ABILITY, not its promise",
                    color=AMBER).scale(0.5).to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        self.you = node(-4.8, 0.4, "your device", "where you type")
        self.server = node(0, 0.4, "their servers", "database + delivery")
        self.them = node(4.8, 0.4, "their device", "where they read")
        self.play(FadeIn(self.you), FadeIn(self.server), FadeIn(self.them))

        wires = VGroup(
            Line(self.you.box.get_right(), self.server.box.get_left(),
                 stroke_color=LINE, stroke_width=2),
            Line(self.server.box.get_right(), self.them.box.get_left(),
                 stroke_color=LINE, stroke_width=2),
        )
        self.play(Create(wires))
        self.wires = wires

        msg = envelope(DANGER, '"see you at 7"', w=3.0)
        msg.move_to(self.you.box.get_center() + DOWN * 1.6)
        self.play(FadeIn(msg))
        self.play(msg.animate.move_to(self.server.box.get_center() + DOWN * 1.6), run_time=1.0)
        readable = Text("readable, right here", color=DANGER).scale(0.4)
        readable.move_to([0, -2.7, 0])
        self.play(FadeIn(readable))
        self.play(msg.animate.move_to(self.them.box.get_center() + DOWN * 1.6), run_time=1.0)
        why = Text('"we would never" describes intent. intent changes.',
                   color=MUTED).scale(0.44).move_to([0, -3.4, 0])
        self.play(FadeIn(why))
        self.hold_until(20)
        self.play(FadeOut(VGroup(msg, readable, why, head)))

    # ── Scene 2 (0:20–0:45), sealed on your device ─────────────────────────
    def beat_sealed(self):
        head = Text("pollis seals it before it leaves your device",
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        keys = VGroup(
            Text("🔑 key stays here", color=OK).scale(0.34).next_to(self.you, UP, buff=0.2),
            Text("🔑 key stays here", color=OK).scale(0.34).next_to(self.them, UP, buff=0.2),
        )
        self.play(FadeIn(keys))

        sealed = envelope(OK, "▓▓▓▓▓▓▓▓▓▓▓▓", w=3.0)
        sealed.move_to(self.you.box.get_center() + DOWN * 1.6)
        self.play(FadeIn(sealed))
        self.play(sealed.animate.move_to(self.server.box.get_center() + DOWN * 1.6),
                  run_time=1.0)
        cant = Text("we pass it along. we never hold the key.",
                    color=OK).scale(0.44).move_to([0, -2.7, 0])
        self.play(FadeIn(cant), Indicate(self.server, color=OK, scale_factor=1.03))
        self.play(sealed.animate.move_to(self.them.box.get_center() + DOWN * 1.6),
                  run_time=1.0)
        opened = envelope(OK, '"see you at 7"', w=3.0)
        opened.move_to(sealed.get_center())
        self.play(FadeOut(sealed), FadeIn(opened))
        post = Text("a post office handling sealed envelopes",
                    color=MUTED).scale(0.44).move_to([0, -3.4, 0])
        self.play(FadeIn(post))
        self.hold_until(45)
        self.play(FadeOut(VGroup(head, keys, opened, cant, post)))

    # ── Scene 3 (0:45–1:15), the outside of the envelope ───────────────────
    def beat_metadata(self):
        head = Text("but look at the OUTSIDE of the envelope",
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))
        self.play(FadeOut(self.you), FadeOut(self.them), FadeOut(self.wires),
                  self.server.animate.move_to([0, 2.4, 0]).scale(0.9))

        env = panel(9.4, 3.0, AMBER).move_to([0, -0.4, 0])
        inside = Text("contents: ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓  (we cannot open this)",
                      font=MONO, color=OK).scale(0.36).move_to([0, -1.4, 0])
        fields = VGroup(
            Text("from      an account we can identify", font=MONO, color=AMBER).scale(0.34),
            Text("when      roughly, to the second", font=MONO, color=AMBER).scale(0.34),
            Text("size      roughly how big it is", font=MONO, color=AMBER).scale(0.34),
            Text("thread    which conversation it belongs to", font=MONO, color=AMBER).scale(0.34),
        ).arrange(DOWN, buff=0.18, aligned_edge=LEFT).move_to([0, 0.15, 0])
        self.play(FadeIn(env), FadeIn(inside))
        for f in fields:
            self.play(FadeIn(f, shift=RIGHT * 0.12), run_time=0.45)
            self.play(Indicate(f, color=AMBER, scale_factor=1.04), run_time=0.35)

        honest = Text("that is metadata. it can be sensitive all by itself,",
                      color=FG).scale(0.46).move_to([0, -2.5, 0])
        honest2 = Text("and we are not going to pretend it doesn't exist.",
                       color=AMBER).scale(0.46).move_to([0, -3.1, 0])
        self.play(FadeIn(honest))
        self.play(FadeIn(honest2))
        self.hold_until(75)
        self.play(FadeOut(VGroup(head, env, inside, fields, honest, honest2, self.server)))

    # ── Scene 4 (1:15–1:45), the attack that actually matters ──────────────
    def beat_key_substitution(self):
        head = Text("the subtler risk: the wrong key for the right person",
                    color=DANGER).scale(0.5).to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        a = node(-4.6, 1.4, "you", "asking for bob's key", w=3.0, h=1.1)
        s = node(0, 1.4, "a server", "hands you a key", w=3.4, h=1.1,
                 color=DANGER, title_color=DANGER)
        b = node(4.6, 1.4, "bob", "has his own key", w=3.0, h=1.1)
        self.play(FadeIn(a), FadeIn(s), FadeIn(b))

        label = envelope(DANGER, "labelled bob, the wrong key", w=6.0, scale=0.34)
        label.move_to([0, -0.1, 0])
        self.play(FadeIn(label))
        opens = Text("now everything opens here, in the middle. "
                     "the maths never broke.", color=DANGER).scale(0.44)
        opens.move_to([0, -0.95, 0])
        self.play(FadeIn(opens))
        self.hold_until(95)

        gates = VGroup(
            node(-3.2, -2.3, "safety numbers", "compare once, in person → topic 6",
                 color=OK, w=6.0, h=1.2, title_color=OK),
            node(3.2, -2.3, "the account-key log", "every key we publish, in public → topic 9",
                 color=OK, w=6.0, h=1.2, title_color=OK),
        )
        self.play(FadeIn(gates[0]))
        self.play(FadeIn(gates[1]))
        shut = Text("both are things YOU can check. the attack fails.",
                    color=OK).scale(0.46).move_to([0, -3.4, 0])
        self.play(FadeIn(shut))
        self.hold_until(110)
        self.play(FadeOut(VGroup(head, a, s, b, label, opens, gates, shut)))

    # ── Scene 5 (1:45–2:15), the boundary diagram (the hero image) ─────────
    def beat_boundary(self):
        head = Text("the whole picture", color=AMBER).scale(0.6).move_to([0, 3.3, 0])
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        green = panel(12.4, 1.55, OK, fill_opacity=0.07, fill=OK).move_to([0, 1.85, 0])
        green_t = VGroup(
            Text("GREEN, we cannot see it", color=OK).scale(0.42),
            Text("message contents · your private keys · anything decrypted on your device",
                 color=MUTED).scale(0.32),
        ).arrange(DOWN, buff=0.12).move_to(green.get_center())

        amber = panel(12.4, 1.55, AMBER, fill_opacity=0.07, fill=AMBER).move_to([0, 0.1, 0])
        amber_t = VGroup(
            Text("AMBER, we CAN see it, and we say so", color=AMBER).scale(0.42),
            Text("who sent · when · roughly how big · which conversation",
                 color=MUTED).scale(0.32),
        ).arrange(DOWN, buff=0.12).move_to(amber.get_center())

        red = panel(12.4, 1.85, DANGER, fill_opacity=0.07, fill=DANGER).move_to([0, -1.85, 0])
        red_t = VGroup(
            Text("RED, outside what any messenger defends", color=DANGER).scale(0.42),
            Text("someone holding your unlocked device · our own bugs",
                 color=MUTED).scale(0.32),
            Text("· a new device starts empty: there is no key backup (topic 4)",
                 color=MUTED).scale(0.32),
        ).arrange(DOWN, buff=0.1).move_to(red.get_center())

        self.play(FadeIn(green), FadeIn(green_t))
        self.play(FadeIn(amber), FadeIn(amber_t))
        self.play(FadeIn(red), FadeIn(red_t))

        close = Text("we shrink what you take on faith, and publish evidence for what's left.",
                     color=FG).scale(0.44).move_to([0, -3.4, 0])
        self.play(FadeIn(close))
        self.hold_until(133)
        self.play(FadeOut(VGroup(head, green, green_t, amber, amber_t, red, red_t, close)))
        self.wait(0.5)
