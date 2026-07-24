"""
Topic 12 — "Reading the artifacts page and the in-app Security page" (#601).

One continuous ~3:00 scene, five beats, NO audio (added later). The dashboards
are drawn as faithful RECREATIONS in the site palette rather than screenshots, so
they stay legible at video scale and don't rot the moment the UI shifts; every
label and verdict string is taken verbatim from the real surfaces.

Render (see learn/manim/render.sh):
    learn/manim/render.sh ReadingTheDashboards reading-the-dashboards m

Accuracy anchors:
  - website/artifacts.html, website/artifacts.js (renderReleaseProofs, vTag)
  - frontend/src/components/Security/BuildVerifyLine.tsx — the four verdict
    strings, verbatim
  - frontend/src/pages/SecurityPage.tsx — the Security page surfaces
  - pollis-core/src/commands/transparency.rs — BuildVerifyStatus semantics
  - PR #587 — the `vv` prefix bug that hid a functional lookup failure
  - Live values: verify.pollis.com binaries head, tree_size 60, root 37945cb2…
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

LIVE_ROOT = "37945cb2f61ee43782259a3893336b8ba8b8679d3af1612742deeec75e46cc0c"
LIVE_SIZE = 60
PINNED_KEY = "175ebfef98fc6b20c67c4cba9d4a36a4f85f05afa4e31f707e7d7e3c02227148"


def panel(width, height, color=LINE):
    return RoundedRectangle(
        corner_radius=0.1, width=width, height=height,
        stroke_color=color, stroke_width=1.8, fill_color=BG, fill_opacity=1.0,
    )


def section(width, height, title, body, color=LINE, title_color=FG):
    box = panel(width, height, color)
    t = Text(title, color=title_color).scale(0.38)
    b = Text(body, color=MUTED).scale(0.3)
    txt = VGroup(t, b).arrange(DOWN, buff=0.14).move_to(box.get_center())
    g = VGroup(box, txt)
    g.box = box
    return g


def tag(text, color, scale=0.3):
    t = Text(text, font=MONO, color=color).scale(scale)
    b = RoundedRectangle(
        corner_radius=0.05, width=t.width + 0.3, height=t.height + 0.2,
        stroke_color=color, stroke_width=1.5, fill_color=BG, fill_opacity=1,
    )
    g = VGroup(b, t)
    g.box = b
    g.label = t
    return g


class ReadingTheDashboards(Scene):
    def construct(self):
        self.beat_tour()
        self.beat_row()
        self.beat_same_root()
        self.beat_verdicts()
        self.beat_closing()

    def hold_until(self, target):
        """Pad to `target` seconds so each beat stays readable without audio and
        matches learn/manim/scripts/reading-the-dashboards.md."""
        now = self.renderer.time
        if target > now:
            self.wait(target - now)

    # ── Scene 1 (0:00–0:40) — the artifacts page, annotated ─────────────────
    def beat_tour(self):
        head = Text("pollis.com/artifacts", color=AMBER).scale(0.55)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        releases = section(9.0, 1.1, "Latest releases",
                           "what we ship right now, and where to get it")
        releases.move_to([-1.4, 2.1, 0])
        self.play(FadeIn(releases))
        self.hold_until(15)

        proofs = section(9.0, 1.5, "Release proofs",
                         "is this version in the binaries log? did the tree verify? "
                         "what's inside?", color=AMBER, title_color=AMBER)
        proofs.move_to([-1.4, 0.55, 0])
        note1 = Text("← the one that matters", color=AMBER).scale(0.34)
        note1.next_to(proofs, RIGHT, buff=0.3)
        self.play(FadeIn(proofs), FadeIn(note1))
        self.hold_until(28)

        audit = section(9.0, 1.1, "Daily transparency self-audit",
                        "the current signed head of all three logs → topic 9")
        audit.move_to([-1.4, -0.85, 0])
        keyb = section(9.0, 1.2, "The one key you trust",
                       PINNED_KEY[:44] + "…  → topic 8", color=OK, title_color=OK)
        keyb.move_to([-1.4, -2.25, 0])
        self.play(FadeIn(audit))
        self.play(FadeIn(keyb))
        caveat = Text("the browser key check on that page is a convenience, "
                      "not a trust anchor", color=MUTED).scale(0.34)
        caveat.move_to([0, -3.3, 0])
        self.play(FadeIn(caveat))
        self.hold_until(40)
        self.play(FadeOut(VGroup(head, releases, proofs, note1, audit, keyb, caveat)))

    # ── Scene 2 (0:40–1:20) — one row, decoded ──────────────────────────────
    def beat_row(self):
        head = Text("one row = one piece of a release", color=AMBER).scale(0.55)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        row = Text("darwin   aarch64   dmg   payload   774c8f…ea92   774c8f…ea92   [included ✓]",
                   font=MONO, color=FG).scale(0.36).move_to([0, 2.0, 0])
        box = RoundedRectangle(corner_radius=0.06, width=row.width + 0.5,
                               height=row.height + 0.35, stroke_color=LINE,
                               stroke_width=1.5).move_to(row.get_center())
        self.play(FadeIn(box), FadeIn(row))

        parts = VGroup(
            tag("platform", MUTED),
            tag("arch", MUTED),
            tag("bundle", MUTED),
            tag("layer → topic 10", AMBER),
            tag("payload hash", MUTED),
            tag("artifact hash", MUTED),
            tag("inclusion tick → topic 8", OK),
        ).arrange(DOWN, buff=0.14).scale(0.82).move_to([-3.6, -0.55, 0])
        for p in parts:
            self.play(FadeIn(p, shift=RIGHT * 0.12), run_time=0.22)
        self.hold_until(55)

        # What the tick actually is: the compressed inclusion climb from Topic 8.
        climb = VGroup(
            Text("hash(entry + sibling)  →", font=MONO, color=FG).scale(0.34),
            Text("hash(that  + sibling)  →", font=MONO, color=FG).scale(0.34),
            Text("hash(that  + sibling)  →  the published root", font=MONO,
                 color=OK).scale(0.34),
        ).arrange(DOWN, buff=0.2, aligned_edge=LEFT).move_to([3.0, -0.4, 0])
        for c in climb:
            self.play(FadeIn(c, shift=UP * 0.1), run_time=0.35)
        meaning = Text("a tick is not a box someone checked — "
                       "it's a proof that was recomputed, and held",
                       color=OK).scale(0.42).move_to([0, -2.8, 0])
        self.play(FadeIn(meaning))
        self.hold_until(70)

        pending = tag("not in log yet", AMBER, scale=0.36).move_to([0, -3.5, 0])
        pending_note = Text("= the daily rebuild hasn't run. almost never an alarm.",
                            color=MUTED).scale(0.34).next_to(pending, RIGHT, buff=0.35)
        self.play(FadeIn(pending), FadeIn(pending_note))
        self.hold_until(80)
        self.play(FadeOut(VGroup(head, box, row, parts, climb, meaning,
                                 pending, pending_note)))

    # ── Scene 3 (1:20–1:50) — the same root, three places ───────────────────
    def beat_same_root(self):
        head = Text("the same sixty-four characters, three places",
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        def place(x, where, how):
            b = panel(4.3, 2.4, OK).move_to([x, 0.4, 0])
            t = VGroup(
                Text(where, color=FG).scale(0.36),
                Text(how, color=MUTED).scale(0.3),
                Text(LIVE_ROOT[:16] + "…", font=MONO, color=AMBER).scale(0.34),
                Text(f"tree_size {LIVE_SIZE}", font=MONO, color=MUTED).scale(0.3),
            ).arrange(DOWN, buff=0.22).move_to(b.get_center())
            return VGroup(b, t)

        a = place(-4.6, "the artifacts page", "read it in a browser")
        b = place(0.0, "curl", "fetch the JSON yourself")
        c = place(4.6, "pollis-verify", "recompute the whole tree")
        self.play(FadeIn(a))
        self.play(FadeIn(b))
        self.play(FadeIn(c))
        eqs = VGroup(
            Text("=", color=OK).scale(0.8).move_to([-2.3, 0.4, 0]),
            Text("=", color=OK).scale(0.8).move_to([2.3, 0.4, 0]),
        )
        self.play(Write(eqs))
        nothing = Text("nothing more needs saying — the picture is the argument",
                       color=MUTED).scale(0.4).move_to([0, -2.2, 0])
        self.play(FadeIn(nothing))
        self.hold_until(110)
        self.play(FadeOut(VGroup(head, a, b, c, eqs, nothing)))

    # ── Scene 4 (1:50–2:45) — the four verdicts ─────────────────────────────
    def beat_verdicts(self):
        head = Text("in the app: Security → This build", color=AMBER).scale(0.55)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        v = section(11.0, 0.95, "Build publicly verified",
                    "this exact binary's fingerprint is in the public log",
                    color=OK, title_color=OK).move_to([0, 2.1, 0])
        p = section(11.0, 0.95, "Build publication pending",
                    "the release isn't in the republished tree yet — normal right "
                    "after a release", color=MUTED).move_to([0, 0.95, 0])
        self.play(FadeIn(v))
        self.play(FadeIn(p))
        self.hold_until(125)

        u = section(11.0, 1.05, "Verification unavailable",
                    "couldn't check: log unreachable, or this release predates the "
                    "exe layer", color=MUTED).move_to([0, -0.3, 0])
        m = section(11.0, 1.05, "Build not in public log",
                    "the tag is published, a comparable fingerprint exists — and "
                    "ours isn't among them", color=DANGER,
                    title_color=DANGER).move_to([0, -1.6, 0])
        self.play(FadeIn(u))
        self.play(FadeIn(m))
        self.hold_until(148)

        diff = VGroup(
            Text('"unavailable"  =  I DON\'T KNOW', color=MUTED).scale(0.46),
            Text('"not in public log"  =  YOU\'VE BEEN TAMPERED WITH',
                 color=DANGER).scale(0.46),
        ).arrange(DOWN, buff=0.22).move_to([0, -2.9, 0])
        self.play(FadeIn(diff))
        self.play(Indicate(u, color=MUTED, scale_factor=1.02),
                  Indicate(m, color=DANGER, scale_factor=1.02), run_time=0.9)
        self.hold_until(158)

        self.play(FadeOut(VGroup(v, p, u, m, diff)))
        confession = VGroup(
            Text("we shipped that distinction wrong once —", color=FG).scale(0.46),
            Text("and every honest macOS build was accused of being fake.",
                 color=DANGER).scale(0.46),
            Text("fixed. and exactly why it's worth labouring.", color=OK).scale(0.44),
        ).arrange(DOWN, buff=0.28).move_to([0, 0.3, 0])
        for ln in confession:
            self.play(FadeIn(ln, shift=UP * 0.12), run_time=0.4)
        self.hold_until(170)
        self.play(FadeOut(VGroup(head, confession)))

    # ── Scene 5 (2:45–3:05) — the closing honesty beat ──────────────────────
    def beat_closing(self):
        head = Text("what green actually means", color=AMBER).scale(0.6)
        head.move_to([0, 2.7, 0])
        self.play(Write(head))

        green = section(11.4, 1.3, "the things Pollis published are consistent, "
                        "permanent,", "and they include the app you are running",
                        color=OK, title_color=OK).move_to([0, 1.2, 0])
        self.play(FadeIn(green))

        nots = VGroup(
            Text("it does NOT mean our code has no bugs.", color=DANGER).scale(0.46),
            Text("it does NOT mean your device is safe.", color=DANGER).scale(0.46),
            Text("it does NOT mean the metadata went anywhere.",
                 color=DANGER).scale(0.46),
        ).arrange(DOWN, buff=0.26).move_to([0, -0.7, 0])
        for ln in nots:
            self.play(FadeIn(ln, shift=UP * 0.1), run_time=0.4)

        close = VGroup(
            Text("we shrank what you have to take on faith,", color=FG).scale(0.48),
            Text("and published the evidence for what's left.", color=FG).scale(0.48),
            Text("now you can check it.", color=AMBER).scale(0.6),
        ).arrange(DOWN, buff=0.22).move_to([0, -2.6, 0])
        self.play(FadeIn(close))
        self.hold_until(185)
        self.play(FadeOut(VGroup(head, green, nots, close)))
        self.wait(0.5)
