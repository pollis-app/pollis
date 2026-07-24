"""
Topic 7, "Merkle trees and append-only logs" (#596), the M0 pilot animation.

One continuous ~2:30 scene, five beats, NO audio (added later). The toy 8-leaf
tree is hashed with real SHA-256 so the "ripple" is truthful, not faked; Scene 2
also surfaces the REAL live root from verify.pollis.com (acceptance criterion).

Render (see learn/manim/render.sh):
    .venv/bin/manim -qh --format=mp4 scenes/merkle_trees.py MerkleTrees

Accuracy anchors:
  - verifiable-log/src/merkle.rs (RFC 6962 leaf/node hashing)
  - docs/transparency.md → "What a transparency log is, in plain terms"
  - Live root: https://verify.pollis.com/v1/binaries/sth/latest.json
"""

import hashlib

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

# ── Palette: mirrors website/styles.css so the video matches the site ────────
BG = "#0f1117"
FG = "#e4e4e7"
MUTED = "#a1a1aa"
AMBER = "#fdba74"
DANGER = "#f1707b"
OK = "#7bd88f"
LINE = "#3f3f46"
MONO = "Cascadia Code, DejaVu Sans Mono, monospace"

config.background_color = BG

# The REAL live binaries-log root + size (verify.pollis.com/v1/binaries/sth),
# fetched 2026-07-23. Shown in Scene 2 so the reader connects toy → real.
LIVE_ROOT = "37945cb2f61ee43782259a3893336b8ba8b8679d3af1612742deeec75e46cc0c"
LIVE_SIZE = 60

# Toy leaves, real identity-style entries, really hashed.
LEAVES = ["id:ariel", "id:boris", "id:chen", "id:diego",
          "id:esra", "id:farah", "id:gita", "id:hana"]


def h(s: str) -> str:
    """Hex SHA-256 of a string, real hashing of toy data."""
    return hashlib.sha256(s.encode()).hexdigest()


def hpair(a: str, b: str) -> str:
    """Parent = hash of the two child hex digests concatenated."""
    return hashlib.sha256((a + b).encode()).hexdigest()


def short(hexd: str) -> str:
    return hexd[:6] + "…"


def chip(hexd: str, color=MUTED, scale=0.42):
    """A small rounded 'fingerprint' chip showing a truncated hex digest."""
    label = Text(short(hexd), font=MONO, color=color).scale(scale)
    box = RoundedRectangle(
        corner_radius=0.06,
        width=label.width + 0.28,
        height=label.height + 0.18,
        stroke_color=color,
        stroke_width=1.5,
        fill_color=BG,
        fill_opacity=1.0,
    )
    g = VGroup(box, label)
    g.hexd = hexd
    g.box = box
    g.label = label
    return g


class MerkleTrees(Scene):
    def construct(self):
        self.beat_problem()
        self.beat_build()
        self.beat_ripple()
        self.beat_leverage()
        self.beat_honest()

    def hold_until(self, target):
        """Pad the scene so cumulative time reaches `target` seconds, keeps each
        beat on screen long enough to READ its captions (no audio yet) and matches
        the narration pacing in learn/manim/scripts/merkle-trees.md."""
        now = self.renderer.time
        if target > now:
            self.wait(target - now)

    # ── Scene 1 (0:00–0:24), the problem ───────────────────────────────────
    def beat_problem(self):
        title = Text("How could you check the list was never changed?",
                     color=AMBER).scale(0.6).to_edge(UP).shift(DOWN * 0.2)

        rows = VGroup()
        for i in range(6):
            label = Text(f"key #{i + 1}", font=MONO, color=FG).scale(0.4)
            bar = RoundedRectangle(
                corner_radius=0.05, width=3.2, height=0.42,
                stroke_color=LINE, stroke_width=1.5, fill_color=BG, fill_opacity=1,
            )
            label.move_to(bar.get_center())
            rows.add(VGroup(bar, label))
        rows.arrange(DOWN, buff=0.14).shift(LEFT * 2.4 + DOWN * 0.3)

        caption = Text("we publish a list and promise to only ever add to it",
                       color=MUTED).scale(0.42)
        caption.next_to(rows, RIGHT, buff=0.8)

        self.play(AddTextLetterByLetter(title, run_time=0.7))
        self.play(FadeIn(rows, shift=UP * 0.2))
        self.play(FadeIn(caption))
        self.wait(0.8)

        # The quiet edit: row #3 flips red while "you aren't looking".
        edited = rows[2]
        watch = Text("...you miss one day...", color=MUTED).scale(0.42)
        watch.next_to(rows, RIGHT, buff=0.8)
        self.play(FadeOut(caption, run_time=0.3), FadeIn(watch, run_time=0.3))
        self.play(
            edited[0].animate.set_stroke(DANGER).set_fill(DANGER, opacity=0.12),
            edited[1].animate.set_color(DANGER),
        )
        quiet = Text("...and one entry is quietly changed.", color=DANGER).scale(0.42)
        quiet.next_to(rows, RIGHT, buff=0.8)
        self.play(FadeOut(watch, run_time=0.3), FadeIn(quiet, run_time=0.3))
        self.wait(1.0)
        self.hold_until(23)
        self.play(FadeOut(VGroup(title, rows, quiet)))

    # ── Scene 2 (0:24–0:52), build the tree, reveal the real root ──────────
    def beat_build(self):
        # Compute the toy tree.
        leaf_h = [h(x) for x in LEAVES]
        l1 = [hpair(leaf_h[0], leaf_h[1]), hpair(leaf_h[2], leaf_h[3]),
              hpair(leaf_h[4], leaf_h[5]), hpair(leaf_h[6], leaf_h[7])]
        l2 = [hpair(l1[0], l1[1]), hpair(l1[2], l1[3])]
        root = hpair(l2[0], l2[1])

        self.tree = {}  # stash for the ripple scene

        # Leaves + their hash chips along the bottom.
        xs = [-6.1 + i * 1.75 for i in range(8)]
        leaf_boxes, leaf_chips = VGroup(), VGroup()
        for i, x in enumerate(xs):
            lb = RoundedRectangle(
                corner_radius=0.05, width=1.35, height=0.5,
                stroke_color=LINE, stroke_width=1.5, fill_color=BG, fill_opacity=1,
            ).move_to([x, -3.1, 0])
            txt = Text(LEAVES[i], font=MONO, color=FG).scale(0.34).move_to(lb.get_center())
            leaf_boxes.add(VGroup(lb, txt))
            c = chip(leaf_h[i], color=MUTED).move_to([x, -2.35, 0])
            leaf_chips.add(c)

        heading = Text("hash each entry, then hash pairs, all the way up",
                       color=AMBER).scale(0.5).to_edge().shift([0, -0.15, 0])
        self.play(AddTextLetterByLetter(heading, run_time=0.7))
        self.play(FadeIn(leaf_boxes, shift=UP * 0.2), run_time=0.8)
        self.play(*[FadeIn(c, shift=UP * 0.2) for c in leaf_chips], run_time=0.8)
        self.wait(0.4)

        def level(hashes, y, child_chips):
            chips, edges = VGroup(), VGroup()
            for j, hd in enumerate(hashes):
                left, rightc = child_chips[2 * j], child_chips[2 * j + 1]
                cx = (left.get_center()[0] + rightc.get_center()[0]) / 2
                c = chip(hd, color=MUTED, scale=0.44 if y < 2 else 0.5).move_to([cx, y, 0])
                edges.add(
                    Line(left.get_top(), c.get_bottom(), stroke_color=LINE, stroke_width=1.5),
                    Line(rightc.get_top(), c.get_bottom(), stroke_color=LINE, stroke_width=1.5),
                )
                chips.add(c)
            self.play(*[Create(e) for e in edges], run_time=0.5)
            self.play(*[FadeIn(c) for c in chips], run_time=0.6)
            return chips, edges

        l1_chips, l1_edges = level(l1, -1.15, leaf_chips)
        l2_chips, l2_edges = level(l2, 0.35, l1_chips)

        # Root, emphasized amber.
        root_chip = chip(root, color=AMBER, scale=0.6).move_to([0, 1.7, 0])
        root_chip.box.set_stroke(AMBER, width=2.5)
        r_edges = VGroup(
            Line(l2_chips[0].get_top(), root_chip.get_bottom(), stroke_color=LINE, stroke_width=1.5),
            Line(l2_chips[1].get_top(), root_chip.get_bottom(), stroke_color=LINE, stroke_width=1.5),
        )
        root_label = Text("the root, one fingerprint of the whole list",
                          color=AMBER).scale(0.4).next_to(root_chip, UP, buff=0.2)
        self.play(*[Create(e) for e in r_edges], run_time=0.5)
        self.play(FadeIn(root_chip), AddTextLetterByLetter(root_label, run_time=0.7))
        self.wait(0.8)

        # Swap to the REAL live root.
        real = VGroup(
            Text("the real thing, right now:", color=MUTED).scale(0.4),
            Text(LIVE_ROOT[:32] + "…", font=MONO, color=AMBER).scale(0.42),
            Text(f"binaries log · {LIVE_SIZE} entries · verify.pollis.com",
                 color=MUTED).scale(0.36),
        ).arrange(DOWN, buff=0.14)
        real.move_to(root_label.get_center()).shift([0, 0.25, 0])
        self.play(FadeOut(root_label), FadeIn(real))
        self.wait(1.0)
        self.hold_until(51)

        self.tree = dict(
            heading=heading, leaf_boxes=leaf_boxes, leaf_chips=leaf_chips,
            l1_chips=l1_chips, l2_chips=l2_chips, root_chip=root_chip,
            edges=VGroup(l1_edges, l2_edges, r_edges), real=real,
            leaf_h=leaf_h, l1=l1, l2=l2, root=root,
        )
        self.play(FadeOut(real), FadeOut(heading))

    # ── Scene 3 (0:52–1:26), the ripple ────────────────────────────────────
    def beat_ripple(self):
        t = self.tree
        heading = Text("change one character, the root changes completely",
                       color=AMBER).scale(0.5).to_edge().shift([0, -0.15, 0])
        self.play(AddTextLetterByLetter(heading, run_time=0.7))

        def ripple(idx, new_leaf, run=1.0):
            new_leaf_h = h(new_leaf)
            j1 = idx // 2
            new_l1 = hpair(
                new_leaf_h if idx == 2 * j1 else t["leaf_h"][2 * j1],
                new_leaf_h if idx == 2 * j1 + 1 else t["leaf_h"][2 * j1 + 1],
            )
            j2 = j1 // 2
            new_l2 = hpair(
                new_l1 if j1 == 2 * j2 else t["l1"][2 * j2],
                new_l1 if j1 == 2 * j2 + 1 else t["l1"][2 * j2 + 1],
            )
            new_root = hpair(
                new_l2 if j2 == 0 else t["l2"][0],
                new_l2 if j2 == 1 else t["l2"][1],
            )

            # Path chips recolor + relabel to danger, bottom-up.
            steps = [
                (t["leaf_chips"][idx], new_leaf_h),
                (t["l1_chips"][j1], new_l1),
                (t["l2_chips"][j2], new_l2),
                (t["root_chip"], new_root),
            ]
            for chip_mob, newh in steps:
                nl = Text(short(newh), font=MONO, color=DANGER).scale(
                    chip_mob.label.font_size / 48).move_to(chip_mob.label.get_center())
                self.play(
                    chip_mob.box.animate.set_stroke(DANGER),
                    FadeOut(chip_mob.label, run_time=0.01),
                    FadeIn(nl, run_time=0.01),
                    run_time=run * 0.35,
                )
                chip_mob.remove(chip_mob.label)
                chip_mob.add(nl)
                chip_mob.label = nl
                self.play(Indicate(chip_mob, color=DANGER, scale_factor=1.15), run_time=run * 0.25)

            # commit new baselines so a second ripple is consistent
            t["leaf_h"][idx] = new_leaf_h
            t["l1"][j1] = new_l1
            t["l2"][j2] = new_l2
            t["root"] = new_root

        cap1 = Text('"id:chen" → "id:chan"', font=MONO, color=DANGER).scale(0.4)
        cap1.to_edge().shift([0, 0.15, 0])
        self.play(FadeIn(cap1))
        ripple(2, "id:chan", run=1.1)
        self.wait(0.6)

        cap2 = Text("again, a different entry, same result", color=MUTED).scale(0.4)
        cap2.move_to(cap1.get_center())
        self.play(FadeOut(cap1), FadeIn(cap2))
        ripple(6, "id:gits", run=0.7)
        self.wait(0.8)

        note = Text("one character, anywhere → a different root",
                    color=FG).scale(0.44).move_to([0, 2.7, 0])
        self.play(FadeOut(cap2), FadeOut(heading), FadeIn(note))
        self.wait(0.8)
        self.hold_until(85)
        self.play(
            FadeOut(note),
            FadeOut(t["leaf_boxes"]), FadeOut(t["leaf_chips"]),
            FadeOut(t["l1_chips"]), FadeOut(t["l2_chips"]),
            FadeOut(t["root_chip"]), FadeOut(t["edges"]),
        )

    # ── Scene 4 (1:26–1:44), the leverage ──────────────────────────────────
    def beat_leverage(self):
        left = VGroup(
            Text("to check the list is unchanged", color=MUTED).scale(0.4),
            Text("download 50,000 entries", color=DANGER).scale(0.6),
            Text("and compare all of them", color=MUTED).scale(0.4),
        ).arrange(DOWN, buff=0.2).shift([-3.4, 0, 0])

        right = VGroup(
            Text("or", color=MUTED).scale(0.4),
            Text("compare 64 characters", color=OK).scale(0.6),
            Text(LIVE_ROOT[:24] + "…", font=MONO, color=AMBER).scale(0.4),
        ).arrange(DOWN, buff=0.2).shift([3.4, 0, 0])

        divider = Line([0, -1.4, 0], [0, 1.4, 0], stroke_color=LINE, stroke_width=1.5)

        self.play(FadeIn(left, shift=RIGHT * 0.2))
        self.play(Create(divider))
        self.play(FadeIn(right, shift=LEFT * 0.2))
        self.wait(1.4)
        self.hold_until(103)
        self.play(FadeOut(VGroup(left, right, divider)))

    # ── Scene 5 (1:44–2:30), what it does, and does not, do ──────────────────────────────
    def beat_honest(self):
        head = Text("what it does, and does not, do", color=AMBER).scale(0.6).move_to([0, 2.6, 0])
        body = VGroup(
            Text("a Merkle tree does not judge whether an entry is truthful.", color=FG).scale(0.46),
            Text("a wrong entry could still be added; the tree just records it.", color=MUTED).scale(0.42),
        ).arrange(DOWN, buff=0.25).shift([0, 1.1, 0])

        self.play(AddTextLetterByLetter(head, run_time=0.7))
        self.play(FadeIn(body))
        self.wait(1.2)

        turn = VGroup(
            Text("what it removes is the ability to change the list quietly.", color=OK).scale(0.5),
            Text("showing one list to you and another to someone else, impossible.",
                 color=MUTED).scale(0.4),
            Text("or changing it later, impossible.", color=MUTED).scale(0.4),
        ).arrange(DOWN, buff=0.22).shift([0, -0.6, 0])
        self.play(FadeIn(turn, shift=UP * 0.2))
        self.wait(1.2)

        # Two far-apart readers compare the same root, agree.
        def reader(x, name):
            dot = RoundedRectangle(corner_radius=0.3, width=0.6, height=0.6,
                                   stroke_color=AMBER, stroke_width=2,
                                   fill_color=BG, fill_opacity=1).move_to([x, -2.6, 0])
            lab = Text(name, color=MUTED).scale(0.34).next_to(dot, DOWN, buff=0.12)
            rt = Text(short(LIVE_ROOT), font=MONO, color=OK).scale(0.36).next_to(dot, UP, buff=0.12)
            return VGroup(dot, lab, rt)

        r1, r2 = reader(-3.5, "reader in Berlin"), reader(3.5, "reader in Seoul")
        eq = Text("=", color=OK).scale(0.8).move_to([0, -2.35, 0])
        self.play(FadeOut(body), FadeOut(turn))
        self.play(FadeIn(r1), FadeIn(r2))
        self.play(Write(eq))
        close = Text('you don\'t have to take our word for it.',
                     color=AMBER).scale(0.5).move_to([0, 0.4, 0])
        close2 = Text("you can just check.", color=FG).scale(0.5).next_to(close, DOWN, buff=0.25)
        self.play(FadeIn(close), FadeIn(close2))
        self.wait(1.6)
        self.hold_until(148)
        self.play(FadeOut(VGroup(head, r1, r2, eq, close, close2)))
        self.wait(0.8)
