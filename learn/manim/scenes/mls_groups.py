"""
Topic 3 — "MLS groups: the ratchet tree, epochs, and commits" (#592).

One continuous ~2:40 scene, five beats, NO audio (added later). The tree is
BUILT on screen, never presented finished, and the naive-vs-MLS cost is shown as
a live counter rather than asserted. The removal beat explicitly shows a message
failing to open for the removed member.

Render (see learn/manim/render.sh):
    learn/manim/render.sh MlsGroups mls-groups m

Accuracy anchors:
  - .codesight/wiki/mls.md → "Core Concepts", "Ordering invariant",
    "How Other Devices Catch Up"
  - pollis-core/src/commands/mls.rs (OpenMLS, RFC 9420)
  - docs/transparency.md → "The commit-log invariant"
Note: MLS does not hide metadata — Topic 1 already said so, and nothing here
implies otherwise.
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

MEMBERS = ["ariel", "boris", "chen", "diego", "esra", "farah", "gita", "hana"]


def panel(width, height, color=LINE, fill_opacity=1.0, fill=BG):
    return RoundedRectangle(
        corner_radius=0.1, width=width, height=height,
        stroke_color=color, stroke_width=1.8,
        fill_color=fill, fill_opacity=fill_opacity,
    )


def chip(text, color=FG, w=1.5, h=0.55, scale=0.3):
    b = panel(w, h, color)
    t = Text(text, font=MONO, color=color).scale(scale).move_to(b.get_center())
    g = VGroup(b, t)
    g.box = b
    g.label = t
    return g


class MlsGroups(Scene):
    def construct(self):
        self.beat_naive()
        self.beat_tree()
        self.beat_update()
        self.beat_removal()
        self.beat_epochs()

    def hold_until(self, target):
        """Pad to `target` seconds so each beat stays readable without audio and
        matches learn/manim/scripts/mls-groups.md."""
        now = self.renderer.time
        if target > now:
            self.wait(target - now)

    # ── Scene 1 (0:00–0:30) — the obvious way, and why it hurts ─────────────
    def beat_naive(self):
        head = Text("the obvious way: one encrypted copy per person",
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        me = chip("you", AMBER, w=1.8).move_to([-5.4, 1.4, 0])
        self.play(FadeIn(me))
        peers = VGroup(*[chip(m, MUTED, w=1.8) for m in MEMBERS[1:5]])
        peers.arrange(DOWN, buff=0.35).move_to([2.6, 1.4, 0])
        self.play(FadeIn(peers))
        copies = VGroup(*[
            Line(me.box.get_right(), p.box.get_left(), stroke_color=DANGER, stroke_width=1.6)
            for p in peers
        ])
        for c in copies:
            self.play(Create(c), run_time=0.25)
        cnt = Text("5 people  →  4 copies of every message",
                   color=FG).scale(0.46).move_to([0, -0.8, 0])
        self.play(FadeIn(cnt))
        self.hold_until(15)

        grow = VGroup(
            Text("10 people   →     9 copies", font=MONO, color=MUTED).scale(0.44),
            Text("100 people  →    99 copies", font=MONO, color=DANGER).scale(0.44),
            Text("1000 people →   999 copies", font=MONO, color=DANGER).scale(0.44),
        ).arrange(DOWN, buff=0.22, aligned_edge=LEFT).move_to([0, -2.1, 0])
        for g in grow:
            self.play(FadeIn(g, shift=RIGHT * 0.15), run_time=0.35)
        leave = Text("and when someone LEAVES, everyone needs new keys — "
                     "everyone coordinating with everyone.", color=DANGER).scale(0.4)
        leave.move_to([0, -3.3, 0])
        self.play(FadeIn(leave))
        self.hold_until(30)
        self.play(FadeOut(VGroup(head, me, peers, copies, cnt, grow, leave)))

    # ── Scene 2 (0:30–1:00) — the tree gets built ───────────────────────────
    def beat_tree(self):
        head = Text("MLS arranges the group as a tree (RFC 9420)",
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        xs = [-6.1 + i * 1.75 for i in range(8)]
        self.leaves = VGroup(*[chip(MEMBERS[i], FG, w=1.5).move_to([xs[i], -2.6, 0])
                               for i in range(8)])
        self.play(FadeIn(self.leaves, shift=UP * 0.2), run_time=0.9)

        def level(y, count, span, color=MUTED):
            row = VGroup()
            for j in range(count):
                cx = (xs[j * span] + xs[j * span + span - 1]) / 2
                row.add(chip("key", color, w=1.2, h=0.5, scale=0.28).move_to([cx, y, 0]))
            return row

        self.l1 = level(-1.2, 4, 2)
        self.l2 = level(0.2, 2, 4)
        self.root = chip("root key", AMBER, w=2.0, h=0.6, scale=0.32).move_to([0, 1.6, 0])

        def wires(lower, upper, per):
            g = VGroup()
            for i, low in enumerate(lower):
                g.add(Line(low.box.get_top(), upper[i // per].box.get_bottom(),
                           stroke_color=LINE, stroke_width=1.4))
            return g

        self.w1 = wires(self.leaves, self.l1, 2)
        self.w2 = wires(self.l1, self.l2, 2)
        self.w3 = wires(self.l2, VGroup(self.root), 2)
        self.play(Create(self.w1), FadeIn(self.l1), run_time=0.8)
        self.play(Create(self.w2), FadeIn(self.l2), run_time=0.8)
        self.play(Create(self.w3), FadeIn(self.root), run_time=0.8)

        rule = Text("the rule: you know the keys on the path from YOUR leaf to the root",
                    color=FG).scale(0.44).move_to([0, 2.7, 0])
        self.play(FadeIn(rule))

        path_a = VGroup(self.leaves[2], self.l1[1], self.l2[0], self.root)
        self.play(*[m.box.animate.set_stroke(AMBER) for m in path_a],
                  *[m.label.animate.set_color(AMBER) for m in path_a])
        self.hold_until(48)

        path_b = VGroup(self.leaves[3], self.l1[1], self.l2[0], self.root)
        self.play(*[m.box.animate.set_stroke(OK) for m in path_b],
                  *[m.label.animate.set_color(OK) for m in path_b])
        shared = Text("two members' paths overlap — that shared part is the whole trick",
                      color=OK).scale(0.42).move_to([0, -3.5, 0])
        self.play(FadeIn(shared))
        self.hold_until(60)
        self.play(FadeOut(rule), FadeOut(shared),
                  *[m.box.animate.set_stroke(MUTED) for m in path_a],
                  *[m.label.animate.set_color(MUTED) for m in path_a],
                  *[m.box.animate.set_stroke(MUTED) for m in path_b],
                  *[m.label.animate.set_color(MUTED) for m in path_b])
        self.head3 = head

    # ── Scene 3 (1:00–1:45) — an update ripples, with a live counter ────────
    def beat_update(self):
        self.play(FadeOut(self.head3))
        head = Text("one update, sent to one sibling branch per level",
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        actor = self.leaves[2]
        self.play(*[actor.box.animate.set_stroke(AMBER),
                    actor.label.animate.set_color(AMBER)])

        # New keys travel UP the path and SIDEWAYS to the sibling subtree.
        steps = [
            (self.l1[1], self.leaves[3], "to your sibling"),
            (self.l2[0], self.l1[0], "to the branch beside you"),
            (self.root, self.l2[1], "to the other half of the group"),
        ]
        counter = Text("messages sent:  1", font=MONO, color=OK).scale(0.46)
        counter.move_to([4.6, 2.6, 0])
        naive = Text("naive would need:  7", font=MONO, color=DANGER).scale(0.42)
        naive.move_to([4.6, 3.15, 0])
        self.play(FadeIn(counter), FadeIn(naive))

        for i, (up, sideways, label) in enumerate(steps):
            self.play(up.box.animate.set_stroke(AMBER), up.label.animate.set_color(AMBER),
                      run_time=0.4)
            arrow = Line(up.box.get_center(), sideways.box.get_center(),
                         stroke_color=AMBER, stroke_width=2)
            cap = Text(label, color=MUTED).scale(0.32).next_to(arrow, DOWN, buff=0.15)
            self.play(Create(arrow), FadeIn(cap), run_time=0.5)
            self.play(Indicate(sideways, color=AMBER, scale_factor=1.12), run_time=0.4)
            self.play(FadeOut(arrow), FadeOut(cap), run_time=0.3)

        self.play(FadeOut(counter), FadeOut(naive))
        compare = VGroup(
            Text("naive:  99 messages", font=MONO, color=DANGER).scale(0.6),
            Text("MLS:     7 messages", font=MONO, color=OK).scale(0.6),
            Text("(for a group of 100 — it's the DEPTH of the tree, not the size)",
                 color=MUTED).scale(0.38),
        ).arrange(DOWN, buff=0.25).move_to([0, 2.75, 0])
        self.play(FadeOut(head), FadeIn(compare))
        self.hold_until(105)
        self.play(FadeOut(compare))

    # ── Scene 4 (1:45–2:15) — removal, and a message that stays sealed ──────
    def beat_removal(self):
        head = Text("removal is the case that matters",
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        gone = self.leaves[6]
        self.play(gone.box.animate.set_stroke(DANGER),
                  gone.label.animate.set_color(DANGER))
        removed_lab = Text("removed", color=DANGER).scale(0.3)
        removed_lab.next_to(gone, DOWN, buff=0.15)
        self.play(gone.animate.set_opacity(0.45), FadeIn(removed_lab))

        # Fresh keys up the vacated path.
        for m in (self.l1[3], self.l2[1], self.root):
            self.play(m.box.animate.set_stroke(OK), m.label.animate.set_color(OK),
                      run_time=0.35)
            self.play(Indicate(m, color=OK, scale_factor=1.1), run_time=0.3)

        msg = chip("the next message ▓▓▓▓▓▓▓▓", OK, w=5.4, h=0.7, scale=0.32)
        msg.move_to([0, 2.6, 0])
        self.play(FadeIn(msg))
        self.play(msg.animate.move_to([0, -3.5, 0]), run_time=1.0)
        sealed = Text("still sealed. they hold the old key, and it opens nothing.",
                      color=DANGER).scale(0.46).move_to([0, 2.6, 0])
        self.play(msg.box.animate.set_stroke(DANGER),
                  msg.label.animate.set_color(DANGER), FadeIn(sealed))
        self.hold_until(135)
        self.play(FadeOut(VGroup(head, msg, sealed, removed_lab, self.leaves, self.l1, self.l2,
                                 self.root, self.w1, self.w2, self.w3)))

    # ── Scene 5 (2:15–2:45) — epochs and commits ────────────────────────────
    def beat_epochs(self):
        head = Text("two words you'll see everywhere: epoch, and commit",
                    color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        blocks = VGroup(*[
            chip(f"epoch {n}", FG, w=2.0, h=0.75, scale=0.32) for n in (5, 6, 7)
        ]).arrange(RIGHT, buff=1.3).move_to([0, 1.5, 0])
        joins = VGroup(*[
            Line(blocks[i].box.get_right(), blocks[i + 1].box.get_left(),
                 stroke_color=LINE, stroke_width=2) for i in range(2)
        ])
        labels = VGroup(
            Text('commit: "add dana"', color=MUTED).scale(0.32).next_to(joins[0], UP, buff=0.15),
            Text('commit: "remove sam"', color=MUTED).scale(0.32).next_to(joins[1], UP, buff=0.15),
        )
        self.play(FadeIn(blocks), Create(joins), FadeIn(labels))
        defn = Text("an epoch is the group's version number. a commit is what makes it tick.",
                    color=FG).scale(0.44).move_to([0, 0.35, 0])
        self.play(FadeIn(defn))
        self.hold_until(150)

        device = chip("a device that missed epoch 6", DANGER, w=6.0, h=0.7, scale=0.34)
        device.move_to([0, -0.9, 0])
        self.play(FadeIn(device))
        stuck = Text("→ out of step. cannot read epoch 7 until it catches up.",
                     color=DANGER).scale(0.44).move_to([0, -1.7, 0])
        self.play(FadeIn(stuck))
        hardest = VGroup(
            Text("keeping that order right — across every device, drop, and reconnect —",
                 color=FG).scale(0.42),
            Text("is the hardest problem in this app.", color=AMBER).scale(0.5),
            Text("which is why every commit goes into a public append-only log. "
                 "that's next.", color=MUTED).scale(0.4),
        ).arrange(DOWN, buff=0.24).move_to([0, -2.9, 0])
        self.play(FadeIn(hardest))
        self.hold_until(165)
        self.play(FadeOut(VGroup(head, blocks, joins, labels, defn, device, stuck, hardest)))
        self.wait(0.5)
