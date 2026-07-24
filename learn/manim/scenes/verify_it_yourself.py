"""
Topic 11, "pollis-verify: verify it yourself" (#600).

One continuous ~2:45 scene, five beats, NO audio (added later). This is the
payoff topic, so it is mostly *screen*: the real static API, a real `curl`, and
the REAL captured output of `pollis-verify release https://verify.pollis.com
v1.5.3` run against the live log on 2026-07-23 (tree_size 60, root 37945cb2…),
the same head Topics 7–9 show.

Render (see learn/manim/render.sh):
    learn/manim/render.sh VerifyItYourself verify-it-yourself m

Accuracy anchors:
  - verifiable-log-serve/src/bin/pollis-verify.rs, POSITIONAL args:
    `pollis-verify release <BASE_URL> <TAG>` (no --base flag)
  - verifiable_log_serve::release::verify_release, the same path the app uses
  - docs/transparency.md → "The static read API (/v1/...)"
  - .github/workflows/verifier-release.yml, standalone binary + pinned key in
    the release notes
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

LIVE_ROOT = "37945cb2f61ee43782259a3893336b8ba8b8679d3af1612742deeec75e46cc0c"
LIVE_SIZE = 60
PINNED_KEY = "175ebfef98fc6b20c67c4cba9d4a36a4f85f05afa4e31f707e7d7e3c02227148"

# Verbatim from the real run (trimmed to fit the frame).
RUN_LINES = [
    ("$ pollis-verify release https://verify.pollis.com v1.5.3", AMBER),
    ("", FG),
    ("Release: v1.5.3", FG),
    ("Found:   yes", FG),
    (f"STH:     tree_size {LIVE_SIZE}  root {LIVE_ROOT[:32]}…", FG),
    ("Artifacts (publish order):", MUTED),
    ("  darwin   aarch64  dmg   payload  774c8f…ea92  [included ✓]", OK),
    ("  darwin   aarch64  dmg   signed   eef7ae…c155  [included ✓]", OK),
    ("  darwin   aarch64  dmg   exe      77fc24…bea5  [included ✓]", OK),
    ("  windows  x86_64   nsis  payload  88547f…7926  [included ✓]", OK),
    ("  linux    x86_64   appimage payload 9b1ab1…e653 [included ✓]", OK),
    ("  …", MUTED),
    ("", FG),
    ("PASS: release binaries tree is valid", OK),
]


def panel(width, height, color=LINE):
    return RoundedRectangle(
        corner_radius=0.1, width=width, height=height,
        stroke_color=color, stroke_width=1.8, fill_color=BG, fill_opacity=1.0,
    )


def card(width, height, title, sub, color=LINE, title_color=FG):
    box = panel(width, height, color)
    t = Text(title, color=title_color).scale(0.4)
    s = Text(sub, color=MUTED).scale(0.3)
    txt = VGroup(t, s).arrange(DOWN, buff=0.14).move_to(box.get_center())
    g = VGroup(box, txt)
    g.box = box
    return g


def file_chip(name, color=FG, w=5.6):
    b = panel(w, 0.55, color)
    t = Text(name, font=MONO, color=color).scale(0.3).move_to(b.get_center())
    g = VGroup(b, t)
    g.box = b
    return g


class VerifyItYourself(Scene):
    def construct(self):
        self.beat_shelf()
        self.beat_curl()
        self.beat_run()
        self.beat_pinned_key()
        self.beat_compare()

    def hold_until(self, target):
        """Pad to `target` seconds so each beat stays readable without audio and
        matches learn/manim/scripts/verify-it-yourself.md."""
        now = self.renderer.time
        if target > now:
            self.wait(target - now)

    # ── Scene 1 (0:00–0:30), the API is a shelf of files ───────────────────
    def beat_shelf(self):
        head = Text("it is barely an API, it's a shelf of files",
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        files = VGroup(
            file_chip("/v1/binaries/sth/latest.json", AMBER),
            file_chip("/v1/binaries/entries.json"),
            file_chip("/v1/binaries/public_key.json", MUTED),
            file_chip("/v1/account-keys/sth/latest.json"),
            file_chip("/verify/release/<tag>"),
        ).arrange(DOWN, buff=0.22).move_to([-3.4, 0.7, 0])
        for f in files:
            self.play(FadeIn(f, shift=RIGHT * 0.15), run_time=0.3)

        warn = Text("← do NOT trust this one: compare it to a key you already have",
                    color=DANGER).scale(0.32).next_to(files[2], RIGHT, buff=0.3)
        static = Text("static JSON. no database. no logic.",
                      color=MUTED).scale(0.4).next_to(files, DOWN, buff=0.35)
        self.play(FadeIn(warn), FadeIn(static))
        self.hold_until(15)

        smart = card(5.6, 1.6, "a server that COMPUTES an answer",
                     "can compute a different one for you",
                     color=DANGER, title_color=DANGER).move_to([3.6, 1.6, 0])
        flat = card(5.6, 1.6, "a server handing out pre-written files",
                    "has a much harder time doing that",
                    color=OK, title_color=OK).move_to([3.6, -0.7, 0])
        self.play(FadeIn(smart))
        self.play(FadeIn(flat))
        audit = Text("and the daily publish job re-reads its own shelf to confirm "
                     "it matches what was signed", color=MUTED).scale(0.36)
        audit.move_to([0, -3.0, 0])
        self.play(FadeIn(audit))
        self.hold_until(30)
        self.play(FadeOut(VGroup(head, files, warn, static, smart, flat, audit)))

    # ── Scene 2 (0:30–1:00), curl it, then find the same root ──────────────
    def beat_curl(self):
        head = Text("fetch it yourself", color=AMBER).scale(0.55)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        term = panel(12.0, 2.6, LINE).move_to([0, 1.3, 0])
        lines = [
            ("$ curl -s https://verify.pollis.com/v1/binaries/sth/latest.json", AMBER),
            ("{", MUTED),
            (f'  "tree_size": {LIVE_SIZE},', FG),
            (f'  "root_hash": "{LIVE_ROOT[:40]}…",', OK),
            ('  "timestamp": 1784738045925,', FG),
            ('  "signature": "f5e206e902c156604ed91a2bd101ae63…"', FG),
            ("}", MUTED),
        ]
        rows = VGroup(*[
            Text(t, font=MONO, color=c).scale(0.34) for t, c in lines
        ]).arrange(DOWN, buff=0.12, aligned_edge=LEFT).move_to(term.get_center())
        rows.align_to(term, LEFT).shift(RIGHT * 0.45)
        self.play(FadeIn(term))
        for r in rows:
            self.play(FadeIn(r, shift=RIGHT * 0.1), run_time=0.22)
        self.hold_until(45)

        site = card(5.6, 1.5, "pollis.com/artifacts", "the same root, published on the site",
                    color=AMBER, title_color=AMBER).move_to([0, -1.3, 0])
        root_txt = Text(LIVE_ROOT[:40] + "…", font=MONO, color=OK).scale(0.34)
        root_txt.move_to([0, -2.4, 0])
        self.play(FadeIn(site), FadeIn(root_txt))
        same = Text("same sixty-four characters. two different places.",
                    color=FG).scale(0.45).move_to([0, -3.2, 0])
        self.play(FadeIn(same), Indicate(rows[3], color=OK, scale_factor=1.05))
        self.hold_until(60)
        self.play(FadeOut(VGroup(head, term, rows, site, root_txt, same)))

    # ── Scene 3 (1:00–1:50), the verifier runs ─────────────────────────────
    def beat_run(self):
        head = Text("files tell you what we CLAIM. the verifier CHECKS it.",
                    color=AMBER).scale(0.5).to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        what = VGroup(
            Text("re-hashes every entry · rebuilds the tree · checks every proof",
                 color=MUTED).scale(0.38),
            Text("and verifies the signature against a key COMPILED INTO IT, not fetched",
                 color=FG).scale(0.4),
        ).arrange(DOWN, buff=0.16).move_to([0, 2.3, 0])
        self.play(FadeIn(what))
        self.hold_until(75)

        term = panel(12.6, 5.0, LINE).move_to([0, -0.7, 0])
        rows = VGroup(*[
            Text(t, font=MONO, color=c).scale(0.32)
            for t, c in RUN_LINES if t
        ]).arrange(DOWN, buff=0.16, aligned_edge=LEFT).move_to(term.get_center())
        rows.align_to(term, LEFT).shift(RIGHT * 0.45)
        self.play(FadeIn(term))
        for r in rows:
            self.play(FadeIn(r, shift=RIGHT * 0.08), run_time=0.16)

        # Annotate the two lines that tie back to earlier topics.
        sth_row = rows[3]
        self.play(Indicate(sth_row, color=AMBER, scale_factor=1.04), run_time=0.7)
        tie1 = Text("← the signed head from topic 8", color=AMBER).scale(0.34)
        tie1.next_to(sth_row, RIGHT, buff=0.3)
        self.play(FadeIn(tie1))
        tie2 = Text("← each tick is an inclusion proof YOUR machine just recomputed",
                    color=OK).scale(0.32)
        tie2.move_to([0, -3.55, 0])
        self.play(FadeIn(tie2))
        self.play(Indicate(rows[-1], color=OK, scale_factor=1.06), run_time=0.8)
        self.hold_until(110)
        self.play(FadeOut(VGroup(head, what, term, rows, tie1, tie2)))

    # ── Scene 4 (1:50–2:20), the pinned key stops the swap ─────────────────
    def beat_pinned_key(self):
        head = Text("suppose a server sent its own key with a matching fake list",
                    color=DANGER).scale(0.5).to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        served = card(6.4, 1.4, "served public_key.json", "9c41a0d7…  (the server's, not the real one)",
                      color=DANGER, title_color=DANGER).move_to([0, 1.5, 0])
        self.play(FadeIn(served))

        binary = panel(7.4, 1.5, OK).move_to([0, -0.6, 0])
        binary_t = VGroup(
            Text("the pollis-verify binary you downloaded (or built)", color=OK).scale(0.36),
            Text(PINNED_KEY[:36] + "…", font=MONO, color=MUTED).scale(0.32),
        ).arrange(DOWN, buff=0.12).move_to(binary.get_center())
        arrow = Line(served.box.get_bottom(), binary.get_top(),
                     stroke_color=DANGER, stroke_width=2)
        self.play(Create(arrow), FadeIn(binary), FadeIn(binary_t))

        stop = Text("served key ≠ compiled-in key   →   it stops. every time.",
                    color=OK).scale(0.5).move_to([0, -2.0, 0])
        self.play(FadeIn(stop), binary.animate.set_stroke(OK, width=3.5))
        build = Text("so if you are seriously auditing us, "
                     "BUILD the verifier yourself", color=AMBER).scale(0.4)
        build.move_to([0, -3.0, 0])
        self.play(FadeIn(build))
        self.hold_until(140)
        self.play(FadeOut(VGroup(head, served, binary, binary_t, arrow, stop, build)))

    # ── Scene 5 (2:20–2:45), compare notes ─────────────────────────────────
    def beat_compare(self):
        head = Text("the most important thing you can do with it",
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))

        def person(x, who):
            b = panel(5.4, 2.2, OK).move_to([x, 0.7, 0])
            t = VGroup(
                Text(who, color=MUTED).scale(0.36),
                Text(f"root {LIVE_ROOT[:16]}…", font=MONO, color=AMBER).scale(0.34),
                Text("PASS", font=MONO, color=OK).scale(0.42),
            ).arrange(DOWN, buff=0.2).move_to(b.get_center())
            return VGroup(b, t)

        a = person(-3.4, "someone in Berlin")
        b = person(3.4, "someone in Seoul")
        self.play(FadeIn(a), FadeIn(b))
        eq = Text("=", color=OK).scale(0.9).move_to([0, 0.7, 0])
        self.play(Write(eq))

        matters = VGroup(
            Text("if those ever differed, two valid signatures over two different trees,",
                 color=DANGER).scale(0.42),
            Text("that is the finding that matters. and the only one you can't get alone.",
                 color=FG).scale(0.45),
        ).arrange(DOWN, buff=0.22).move_to([0, -1.7, 0])
        self.play(FadeIn(matters))
        close = Text("so run it. and compare with someone.",
                     color=AMBER).scale(0.55).move_to([0, -3.0, 0])
        self.play(FadeIn(close))
        self.hold_until(165)
        self.play(FadeOut(VGroup(head, a, b, eq, matters, close)))
        self.wait(0.5)
