"""
Identity topic, "Losing a device, and getting back in".

One continuous ~1:50 scene, four beats, NO audio (added later). Sentence case,
positive framing. Grounded in the real design: PIN unlocks the device, the Secret
Key recovers the account (server holds the account key wrapped in a box only the
Secret Key opens), new devices join by approval or by Secret Key, and losing both
is a deliberate fresh start with no backdoor.

Render:
    learn/manim/render.sh Recovery recovery m

Accuracy anchors:
  - website/security.html §"The PIN protects the device. The Secret Key protects
    your account." (A3-… format, HKDF-SHA256 + AES-256-GCM wrap, external commit)
  - pollis-core/src/commands/auth.rs (secret key, wrap, account_recovery)
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


def card(x, y, title, sub, color=LINE, w=5.4, h=1.5, title_color=FG):
    box = panel(w, h, color).move_to([x, y, 0])
    t = Text(title, color=title_color).scale(0.42)
    s = Text(sub, color=MUTED).scale(0.3)
    txt = VGroup(t, s).arrange(DOWN, buff=0.14).move_to(box.get_center())
    g = VGroup(box, txt)
    g.box = box
    return g


def note(text, color=FG, w=6.4, h=0.8, scale=0.34, mono=False):
    b = panel(w, h, color)
    kw = {"font": MONO} if mono else {}
    t = Text(text, color=color, **kw).scale(scale).move_to(b.get_center())
    g = VGroup(b, t)
    g.box = b
    g.label = t
    return g


class Recovery(Scene):
    def construct(self):
        self.beat_two_things()
        self.beat_the_box()
        self.beat_two_paths()
        self.beat_the_limit()

    def hold_until(self, target):
        now = self.renderer.time
        if target > now:
            self.wait(target - now)

    # ── Scene 1 (0:00–0:25) — PIN vs Secret Key ─────────────────────────────
    def beat_two_things(self):
        head = Text("Two different keys, two different jobs", color=AMBER).scale(0.55)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        pin = card(-3.4, 0.7, "Your PIN", "unlocks this one device",
                   color=OK, title_color=OK, w=5.4, h=1.6)
        sk = card(3.4, 0.7, "Your Secret Key", "recovers your whole account",
                  color=AMBER, title_color=AMBER, w=5.4, h=1.6)
        self.play(FadeIn(pin), FadeIn(sk))

        pin_note = Text("Local to the device. We never see it.",
                        color=MUTED).scale(0.36).move_to([-3.4, -1.2, 0])
        sk_note = Text("Shown once. Write it down like a house deed.",
                       color=MUTED).scale(0.36).move_to([3.4, -1.2, 0])
        self.play(FadeIn(pin_note), FadeIn(sk_note))
        example = note("A3-9XK4Q-7MTP2-R6WHD-3JYFN-B8QVS-5LZCE", AMBER,
                       w=8.4, h=0.7, scale=0.34, mono=True).move_to([0, -2.7, 0])
        self.play(FadeIn(example))
        self.hold_until(25)
        self.play(FadeOut(VGroup(head, pin, sk, pin_note, sk_note, example)))

    # ── Scene 2 (0:25–0:55) — the box on the server ─────────────────────────
    def beat_the_box(self):
        head = Text("How recovery works", color=AMBER).scale(0.6)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        server = card(0, 1.6, "The server", "holds your account key, locked",
                      color=MUTED, w=6.4, h=1.3)
        box = note("account key  ▓▓▓▓▓▓▓▓▓▓  (sealed)", MUTED, w=7.4, h=0.8,
                   scale=0.34, mono=True).move_to([0, 0.1, 0])
        self.play(FadeIn(server), FadeIn(box))

        key_line = Text("Only your Secret Key opens this box. The server does not have it.",
                        color=FG).scale(0.44).move_to([0, -1.1, 0])
        self.play(FadeIn(key_line))
        self.play(box.box.animate.set_stroke(OK),
                  box.label.animate.set_color(OK),
                  Indicate(box, color=OK, scale_factor=1.05))
        opened = note("account key  a91f… (yours again)", OK, w=7.4, h=0.8,
                      scale=0.34, mono=True).move_to([0, 0.1, 0])
        self.play(FadeOut(box), FadeIn(opened))
        cap = Text("So the company cannot open your account, and neither can anyone who steals the box.",
                   color=OK).scale(0.4).move_to([0, -2.4, 0])
        self.play(FadeIn(cap))
        self.hold_until(55)
        self.play(FadeOut(VGroup(head, server, opened, key_line, cap)))

    # ── Scene 3 (0:55–1:25) — two ways to add a device ──────────────────────
    def beat_two_paths(self):
        head = Text("Adding a new device", color=AMBER).scale(0.6)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        a = card(-3.4, 1.2, "Approve from another device",
                 "compare a 6-digit code, and they agree privately",
                 color=OK, title_color=OK, w=6.2, h=1.6)
        b = card(3.4, 1.2, "Or use your Secret Key",
                 "the same recovery, without a second device",
                 color=AMBER, title_color=AMBER, w=6.2, h=1.6)
        self.play(FadeIn(a))
        self.play(FadeIn(b))

        joined = note("The new device then joins every group you are in",
                      FG, w=8.6, h=0.8, scale=0.36).move_to([0, -0.9, 0])
        self.play(FadeIn(joined))
        empty = Text("It starts from today forward, with no old history (that is Topic 4).",
                     color=MUTED).scale(0.38).move_to([0, -2.1, 0])
        self.play(FadeIn(empty))
        self.hold_until(85)
        self.play(FadeOut(VGroup(head, a, b, joined, empty)))

    # ── Scene 4 (1:25–1:50) — the deliberate limit ──────────────────────────
    def beat_the_limit(self):
        head = Text("The one hard rule", color=AMBER).scale(0.6)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        line1 = Text("Lose your Secret Key and every signed-in device,",
                     color=FG).scale(0.5).move_to([0, 1.2, 0])
        line2 = Text("and the account starts fresh.", color=FG).scale(0.5).move_to([0, 0.6, 0])
        self.play(FadeIn(line1))
        self.play(FadeIn(line2))

        why = note("No one holds a spare key to your account, including us.",
                   OK, w=9.2, h=0.9, scale=0.4).move_to([0, -0.9, 0])
        self.play(FadeIn(why))
        close = Text("That is the price of a door only you can open, and it is the point.",
                     color=AMBER).scale(0.46).move_to([0, -2.3, 0])
        self.play(FadeIn(close))
        self.hold_until(110)
        self.play(FadeOut(VGroup(head, line1, line2, why, close)))
        self.wait(0.4)
