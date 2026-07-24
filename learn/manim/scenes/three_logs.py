"""
Topic 9, "Pollis's three logs, and what each one prevents" (#598).

One continuous ~3:00 scene, six beats, NO audio (added later). Each log is
introduced by the ATTACK it closes, not by its data model (acceptance criterion),
and domain separation is shown as a physical shape mismatch rather than asserted.

Render (see learn/manim/render.sh):
    learn/manim/render.sh ThreeLogs three-logs m

Accuracy anchors:
  - docs/transparency.md → "Three domain-separated trees", "The commit-log invariant"
  - verifiable-log-builder/src/binaries.rs      STH_CONTEXT = …:sth:v1:binaries
  - verifiable-log-builder/src/account_key.rs   STH_CONTEXT = …:sth:v1:account-keys
  - verifiable-log/src/sth.rs                   STH_DOMAIN  = …:sth:v1 (commit log, frozen)
  - .github/workflows/transparency-publish.yml  daily republish at 06:47 UTC
  - pollis-core/src/commands/transparency.rs    AuditStatus::Pending (not an alarm)
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
    Polygon,
    RoundedRectangle,
    Scene,
    Text,
    VGroup,
    Write,
    config,
)

# ── Palette: mirrors website/styles.css ─────────────────────────────────────
BG = "#0f1117"
FG = "#e4e4e7"
MUTED = "#a1a1aa"
AMBER = "#fdba74"
DANGER = "#f1707b"
OK = "#7bd88f"
LINE = "#3f3f46"
MONO = "Cascadia Code, DejaVu Sans Mono, monospace"

config.background_color = BG

PINNED_KEY = "175ebfef98fc6b20c67c4cba9d4a36a4f85f05afa4e31f707e7d7e3c02227148"

# The real domain-separation contexts each tree signs under.
CTX_COMMITS = "pollis-verifiable-log:sth:v1"
CTX_ACCOUNT = "pollis-verifiable-log:sth:v1:account-keys"
CTX_BINARIES = "pollis-verifiable-log:sth:v1:binaries"


def panel(width, height, color=LINE, fill=BG, fill_opacity=1.0):
    return RoundedRectangle(
        corner_radius=0.1, width=width, height=height,
        stroke_color=color, stroke_width=1.8,
        fill_color=fill, fill_opacity=fill_opacity,
    )


def labelled(width, height, title, sub, color=LINE, title_color=FG):
    box = panel(width, height, color)
    t = Text(title, color=title_color).scale(0.42)
    s = Text(sub, color=MUTED).scale(0.32)
    txt = VGroup(t, s).arrange(DOWN, buff=0.14).move_to(box.get_center())
    g = VGroup(box, txt)
    g.box = box
    return g


def block(text, color=FG, w=1.25, h=0.7, scale=0.36):
    b = panel(w, h, color)
    t = Text(text, font=MONO, color=color).scale(scale).move_to(b.get_center())
    g = VGroup(b, t)
    g.box = b
    g.label = t
    return g


class ThreeLogs(Scene):
    def construct(self):
        self.beat_shelves()
        self.beat_account_keys()
        self.beat_commit_log()
        self.beat_binaries()
        self.beat_domain_separation()
        self.beat_limits()

    def hold_until(self, target):
        """Pad to `target` seconds so each beat stays readable without audio and
        matches learn/manim/scripts/three-logs.md."""
        now = self.renderer.time
        if target > now:
            self.wait(target - now)

    # ── Scene 1 (0:00–0:20), three shelves, one seal ───────────────────────
    def beat_shelves(self):
        site = labelled(5.0, 0.9, "verify.pollis.com", "one static site, no login",
                        color=AMBER, title_color=AMBER).move_to([0, 2.9, 0])
        self.play(FadeIn(site))

        seal = panel(4.6, 0.75, OK).move_to([0, 1.75, 0])
        seal_t = VGroup(
            Text("one signing key, pinned in the client", color=OK).scale(0.34),
            Text(PINNED_KEY[:24] + "…", font=MONO, color=MUTED).scale(0.3),
        ).arrange(DOWN, buff=0.08).move_to(seal.get_center())
        self.play(FadeIn(seal), FadeIn(seal_t))

        logs = VGroup(
            labelled(4.1, 2.0, "account keys", "who your key really is"),
            labelled(4.1, 2.0, "MLS commits", "what order a group moved in"),
            labelled(4.1, 2.0, "binaries", "which bytes we shipped"),
        ).arrange(RIGHT, buff=0.4).move_to([0, 0.05, 0])
        for lg in logs:
            self.play(FadeIn(lg, shift=UP * 0.15), run_time=0.4)

        stems = VGroup(*[
            Line(seal.get_bottom(), lg.box.get_top(), stroke_color=LINE, stroke_width=1.5)
            for lg in logs
        ])
        self.play(Create(stems), run_time=0.6)

        note = Text("three append-only logs, each one closes a different attack",
                    color=MUTED).scale(0.42).move_to([0, -1.6, 0])
        self.play(FadeIn(note))
        self.hold_until(20)
        self.play(FadeOut(VGroup(site, seal, seal_t, logs, stems, note)))

    # ── Scene 2 (0:20–1:00), account keys ──────────────────────────────────
    def beat_account_keys(self):
        head = Text("log 1, account keys", color=AMBER).scale(0.55)
        head.to_edge(UP).shift(DOWN * 0.1)
        stops = Text("protects against the wrong key being used for you",
                     color=FG).scale(0.45).next_to(head, DOWN, buff=0.25)
        self.play(AddTextLetterByLetter(head, run_time=0.7), FadeIn(stops))

        # The substitution attempt, compressed, the reader met it in Topic 6.
        you = block("you", MUTED, w=1.6, h=0.7, scale=0.34).move_to([-4.8, 0.9, 0])
        us = block("us", DANGER, w=1.6, h=0.7, scale=0.34).move_to([0, 0.9, 0])
        real = block("ariel's real key", OK, w=3.0, h=0.6, scale=0.3).move_to([4.4, 1.6, 0])
        fake = block("a key we control", DANGER, w=3.0, h=0.6, scale=0.3).move_to([4.4, 0.3, 0])
        arrow = Line(us.get_left(), you.get_right(), stroke_color=DANGER, stroke_width=2)
        self.play(FadeIn(you), FadeIn(us), FadeIn(real), FadeIn(fake))
        self.play(Create(arrow), Indicate(fake, color=DANGER, scale_factor=1.1), run_time=0.8)

        ledger = panel(11.0, 1.5, LINE).move_to([0, -1.4, 0])
        ledger_t = Text("the published account-key log", color=MUTED).scale(0.34)
        ledger_t.next_to(ledger, UP, buff=0.12)
        rows = VGroup(*[
            block(f"ariel v{v}", MUTED, w=1.9, h=0.55, scale=0.3) for v in (1, 2, 3)
        ]).arrange(RIGHT, buff=0.3).move_to([-2.4, -1.4, 0])
        self.play(FadeIn(ledger), FadeIn(ledger_t), FadeIn(rows))

        landed = block("ariel v4  ← the fake", DANGER, w=3.4, h=0.55, scale=0.3)
        landed.move_to([2.4, -1.4, 0])
        self.play(FadeIn(landed, shift=DOWN * 0.3))
        indelible = Text("any published key is permanent and public",
                         color=DANGER).scale(0.4).move_to([0, -2.6, 0])
        self.play(FadeIn(indelible))
        watcher = Text("…where ariel's own device checks it   →   flagged",
                       color=OK).scale(0.42).move_to([0, -3.2, 0])
        self.play(FadeIn(watcher))
        self.hold_until(40)

        # The invariant: versions only ever move forward.
        self.play(FadeOut(VGroup(you, us, real, fake, arrow, indelible, watcher)))
        rule = Text("the rule: versions only ever move forward",
                    color=AMBER).scale(0.5).move_to([0, 1.4, 0])
        self.play(FadeIn(rule))
        attempt = block("re-insert ariel v2", DANGER, w=3.6, h=0.6, scale=0.32)
        attempt.move_to([0, 0.4, 0])
        self.play(FadeIn(attempt))
        self.play(attempt.animate.move_to([0, -0.55, 0]), run_time=0.7)
        bounce = Text("rejected, history cannot be renumbered",
                      color=DANGER).scale(0.42).move_to([0, -0.5, 0])
        self.play(attempt.animate.move_to([0, 0.4, 0]).set_opacity(0.35), FadeIn(bounce))
        self.hold_until(60)
        self.play(FadeOut(VGroup(head, stops, ledger, ledger_t, rows, landed,
                                 rule, attempt, bounce)))

    # ── Scene 3 (1:00–1:45), the MLS commit log ────────────────────────────
    def beat_commit_log(self):
        head = Text("log 2, MLS commits", color=AMBER).scale(0.55)
        head.to_edge(UP).shift(DOWN * 0.1)
        stops = Text("protects a group's history from being rewritten",
                     color=FG).scale(0.45).next_to(head, DOWN, buff=0.25)
        self.play(AddTextLetterByLetter(head, run_time=0.7), FadeIn(stops))

        chain = VGroup(*[block(f"epoch {n}", MUTED, w=1.7, h=0.7, scale=0.32)
                         for n in (5, 6, 7)]).arrange(RIGHT, buff=0.7)
        chain.move_to([-3.4, 1.0, 0])
        joins = VGroup(*[
            Line(chain[i].get_right(), chain[i + 1].get_left(),
                 stroke_color=LINE, stroke_width=2) for i in range(2)
        ])
        self.play(FadeIn(chain), Create(joins), run_time=0.9)
        order = Text("a group moves forward in epochs, every member applies the "
                     "same commits, in the same order", color=MUTED).scale(0.42)
        order.move_to([0, -0.4, 0])
        self.play(FadeIn(order))
        self.hold_until(15 + 60)

        # The fork attempt: two different commits both claiming epoch 8.
        up8 = block("epoch 8  (A)", DANGER, w=2.2, h=0.7, scale=0.32).move_to([0.4, 2.0, 0])
        dn8 = block("epoch 8  (B)", DANGER, w=2.2, h=0.7, scale=0.32).move_to([0.4, 0.0, 0])
        fa = Line(chain[2].get_right(), up8.get_left(), stroke_color=DANGER, stroke_width=2)
        fb = Line(chain[2].get_right(), dn8.get_left(), stroke_color=DANGER, stroke_width=2)
        self.play(Create(fa), Create(fb), FadeIn(up8), FadeIn(dn8))
        half_a = Text("half the group here", color=DANGER).scale(0.34).next_to(up8, RIGHT, buff=0.4)
        half_b = Text("half the group here", color=DANGER).scale(0.34).next_to(dn8, RIGHT, buff=0.4)
        self.play(FadeIn(half_a), FadeIn(half_b))
        forked = Text("a fork: two realities, both claiming to be the same group",
                      color=DANGER).scale(0.42).move_to([0, -1.1, 0])
        self.play(FadeOut(order), FadeIn(forked))
        self.hold_until(32 + 60)

        rule = Text("the invariant: one epoch, one commit, the second is rejected",
                    color=OK).scale(0.45).move_to([0, -1.1, 0])
        self.play(FadeOut(forked), FadeIn(rule),
                  dn8.box.animate.set_stroke(LINE), dn8.label.animate.set_color(LINE),
                  fb.animate.set_stroke(LINE), half_b.animate.set_color(LINE))
        self.play(Indicate(up8, color=OK, scale_factor=1.08), run_time=0.6)

        # And what is actually stored: sealed bytes.
        sealed = panel(5.4, 1.15, LINE).move_to([0, -2.5, 0])
        sealed_t = VGroup(
            Text("epoch 8  ·  2026-07-19T04:11Z  ·  group 4f2a…", font=MONO,
                 color=MUTED).scale(0.32),
            Text("payload: ██████████████████  (sealed)", font=MONO, color=FG).scale(0.32),
        ).arrange(DOWN, buff=0.12).move_to(sealed.get_center())
        self.play(FadeIn(sealed), FadeIn(sealed_t))
        seal_note = Text("order is public. content never is, we never had the keys either.",
                         color=AMBER).scale(0.42).move_to([0, -3.4, 0])
        self.play(FadeIn(seal_note))
        self.hold_until(45 + 60)
        self.play(FadeOut(VGroup(head, stops, chain, joins, up8, dn8, fa, fb,
                                 half_a, half_b, rule, sealed, sealed_t, seal_note)))

    # ── Scene 4 (1:45–2:10), binaries ──────────────────────────────────────
    def beat_binaries(self):
        head = Text("log 3, binaries", color=AMBER).scale(0.55)
        head.to_edge(UP).shift(DOWN * 0.1)
        stops = Text("protects against a tampered app made for one person",
                     color=FG).scale(0.45).next_to(head, DOWN, buff=0.25)
        self.play(AddTextLetterByLetter(head, run_time=0.7), FadeIn(stops))

        build = block("a special build, just for you", DANGER, w=5.4, h=0.7, scale=0.34)
        build.move_to([0, 1.5, 0])
        self.play(FadeIn(build))

        left = labelled(5.2, 1.9, "its fingerprint IS in the log",
                        "everyone can see we shipped it", color=OK, title_color=OK)
        right = labelled(5.2, 1.9, "its fingerprint is MISSING",
                        "your own app notices immediately", color=OK, title_color=OK)
        left.move_to([-3.2, -0.4, 0])
        right.move_to([3.2, -0.4, 0])
        la = Line(build.get_bottom(), left.box.get_top(), stroke_color=LINE, stroke_width=1.5)
        ra = Line(build.get_bottom(), right.box.get_top(), stroke_color=LINE, stroke_width=1.5)
        self.play(Create(la), Create(ra), FadeIn(left), FadeIn(right))

        both = Text("there is no third option, both branches end in detection",
                    color=AMBER).scale(0.45).move_to([0, -2.2, 0])
        self.play(FadeIn(both))
        onward = Text("(topic 10 takes this apart layer by layer)",
                      color=MUTED).scale(0.38).move_to([0, -2.9, 0])
        self.play(FadeIn(onward))
        self.hold_until(130)
        self.play(FadeOut(VGroup(head, stops, build, left, right, la, ra, both, onward)))

    # ── Scene 5 (2:10–2:40), domain separation ─────────────────────────────
    def beat_domain_separation(self):
        head = Text("why three logs, and not one big one?", color=AMBER).scale(0.55)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(head, run_time=0.7))
        because = Text("because a signed head for one must never work as a head for another",
                       color=FG).scale(0.45).move_to([0, 2.1, 0])
        self.play(FadeIn(because))
        self.hold_until(145)

        # The head we hold, tagged with the commit-log context.
        card = panel(6.0, 1.5, AMBER).move_to([-3.4, 0.5, 0])
        card_t = VGroup(
            Text("a valid head from the commit log", color=MUTED).scale(0.32),
            Text(CTX_COMMITS, font=MONO, color=AMBER).scale(0.3),
        ).arrange(DOWN, buff=0.14).move_to(card.get_center())
        # Its "tooth", the shape the signature commits to.
        tooth = Polygon([-3.4 - 0.5, -0.25, 0], [-3.4 + 0.5, -0.25, 0], [-3.4, -0.75, 0],
                        stroke_color=AMBER, stroke_width=2,
                        fill_color=AMBER, fill_opacity=0.12)
        self.play(FadeIn(card), FadeIn(card_t), Create(tooth))

        # The binaries ledger, whose slot is a different shape entirely.
        slot_box = panel(6.0, 1.5, LINE).move_to([3.4, -0.9, 0])
        slot_t = VGroup(
            Text("the binaries log expects", color=MUTED).scale(0.32),
            Text(CTX_BINARIES, font=MONO, color=FG).scale(0.3),
        ).arrange(DOWN, buff=0.14).move_to(slot_box.get_center())
        notch = RoundedRectangle(corner_radius=0.05, width=1.1, height=0.5,
                                 stroke_color=LINE, stroke_width=2,
                                 fill_color=BG, fill_opacity=1).move_to([3.4, 0.1, 0])
        self.play(FadeIn(slot_box), FadeIn(slot_t), Create(notch))

        # Try to slot it in, the shapes don't match.
        group = VGroup(card, card_t, tooth)
        self.play(group.animate.shift(RIGHT * 6.8), run_time=1.0)
        self.play(group.animate.shift(LEFT * 0.9), run_time=0.35)
        reject = Text("it does not fit, the signature commits to WHICH tree it belongs to",
                      color=DANGER).scale(0.44).move_to([0, -2.3, 0])
        self.play(FadeIn(reject),
                  tooth.animate.set_stroke(DANGER).set_fill(DANGER, opacity=0.15))
        third = Text(f"account keys sign under   {CTX_ACCOUNT}", font=MONO,
                     color=MUTED).scale(0.32).move_to([0, -3.1, 0])
        self.play(FadeIn(third))
        self.hold_until(160)
        self.play(FadeOut(VGroup(head, because, group, slot_box, slot_t, notch,
                                 reject, third)))

    # ── Scene 6 (2:40–3:00), what these logs do, and do not, do ─────────────────────────────
    def beat_limits(self):
        head = Text("what these logs do, and do not, do", color=AMBER).scale(0.6).move_to([0, 2.6, 0])
        self.play(AddTextLetterByLetter(head, run_time=0.7))
        lines = VGroup(
            Text("these make tampering permanent and public, not impossible.", color=FG).scale(0.5),
            Text("a wrong entry could be written to any of them,", color=MUTED).scale(0.4),
            Text("it would just be permanent, public, and the same for everyone.",
                 color=MUTED).scale(0.4),
            Text("they prove what was published, not that it is bug-free.",
                 color=FG).scale(0.5),
        ).arrange(DOWN, buff=0.28).move_to([0, 0.9, 0])
        for ln in lines:
            self.play(FadeIn(ln, shift=UP * 0.12), run_time=0.4)
        self.hold_until(172)

        clock = labelled(9.6, 1.6, "the logs rebuild once a day, 06:47 UTC",
                         'so a brand-new release can be genuinely absent for a while',
                         color=OK, title_color=OK).move_to([0, -1.9, 0])
        pending = Text('your app says "pending", not "alarm", '
                       'not in the log YET is not the same as not in the log',
                       color=AMBER).scale(0.42).move_to([0, -3.1, 0])
        self.play(FadeIn(clock))
        self.play(FadeIn(pending))
        self.hold_until(180)
        self.play(FadeOut(VGroup(head, lines, clock, pending)))
        self.wait(0.5)
