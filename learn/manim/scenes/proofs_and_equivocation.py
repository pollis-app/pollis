"""
Topic 8, "Signed tree heads, inclusion and consistency proofs, and
equivocation" (#597).

One continuous ~3:05 scene, five beats, NO audio (added later). The 8-leaf toy
tree is the SAME one as Topic 7 and is hashed with real SHA-256, so the audit
path, the verified root, and the forged-entry mismatch are all genuine numbers.
Scene 3 uses the real live STH from verify.pollis.com and the real pinned key.

Render (see learn/manim/render.sh):
    learn/manim/render.sh ProofsAndEquivocation proofs-and-equivocation m

Accuracy anchors:
  - verifiable-log/src/proof.rs (verify_inclusion_proof, verify_consistency_proof)
  - verifiable-log/src/sth.rs (Sth::create_with_context, verify_with_context,
    is_equivocation, same tree_size + different root == proof of equivocation)
  - pollis-core/src/commands/transparency.rs (PINNED_LOG_PUBLIC_KEY,
    served_key_matches_pin, a served key that differs is a hard ALARM)
  - verifiable-log-builder/src/binaries.rs → STH_CONTEXT (domain separation)
  - .github/workflows/transparency-publish.yml (post-publish self-audit +
    across-run equivocation tripwire)
  - docs/transparency.md → "Trust model (the load-bearing idea)"
  - Live STH: https://verify.pollis.com/v1/binaries/sth/latest.json
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
    Polygon,
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

# Real live binaries-log head (verify.pollis.com/v1/binaries/sth/latest.json),
# fetched 2026-07-23, the same head Topic 7 shows.
LIVE_ROOT = "37945cb2f61ee43782259a3893336b8ba8b8679d3af1612742deeec75e46cc0c"
LIVE_SIZE = 60
LIVE_TS = "2026-07-18T…Z"
LIVE_SIG = "f5e206e902c156604ed91a2bd101ae63468946f6989a116e18554ca70bf1ba45"

# The real pinned Ed25519 key, pollis-core/src/commands/transparency.rs.
PINNED_KEY = "175ebfef98fc6b20c67c4cba9d4a36a4f85f05afa4e31f707e7d7e3c02227148"

# Same toy leaves as Topic 7, so the reader recognises the tree.
LEAVES = ["id:ariel", "id:boris", "id:chen", "id:diego",
          "id:esra", "id:farah", "id:gita", "id:hana"]
PROVEN = 2          # the leaf we prove: "id:chen"
FORGED = "id:mallory"


def h(s: str) -> str:
    return hashlib.sha256(s.encode()).hexdigest()


def hpair(a: str, b: str) -> str:
    return hashlib.sha256((a + b).encode()).hexdigest()


def short(hexd: str, n: int = 6) -> str:
    return hexd[:n] + "…"


def chip(hexd: str, color=MUTED, scale=0.42, label_text=None):
    """A small rounded 'fingerprint' chip showing a truncated hex digest."""
    label = Text(label_text or short(hexd), font=MONO, color=color).scale(scale)
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


def recolor(mob, color):
    """Recolor a chip's box + label in place (animation-friendly pair)."""
    return [mob.box.animate.set_stroke(color), mob.label.animate.set_color(color)]


def panel(width, height, color=LINE):
    return RoundedRectangle(
        corner_radius=0.1, width=width, height=height,
        stroke_color=color, stroke_width=1.5, fill_color=BG, fill_opacity=1.0,
    )


class ProofsAndEquivocation(Scene):
    def construct(self):
        self.beat_inclusion()
        self.beat_consistency()
        self.beat_sth()
        self.beat_equivocation()
        self.beat_chain()

    def hold_until(self, target):
        """Pad the scene so cumulative time reaches `target` seconds, keeps each
        beat on screen long enough to READ its captions (no audio yet) and matches
        the narration pacing in learn/manim/scripts/proofs-and-equivocation.md."""
        now = self.renderer.time
        if target > now:
            self.wait(target - now)

    # ── Scene 1 (0:00–0:50), the inclusion proof ───────────────────────────
    def beat_inclusion(self):
        leaf_h = [h(x) for x in LEAVES]
        l1 = [hpair(leaf_h[0], leaf_h[1]), hpair(leaf_h[2], leaf_h[3]),
              hpair(leaf_h[4], leaf_h[5]), hpair(leaf_h[6], leaf_h[7])]
        l2 = [hpair(l1[0], l1[1]), hpair(l1[2], l1[3])]
        root = hpair(l2[0], l2[1])

        heading = Text("is MY entry in the tree?", color=AMBER).scale(0.55)
        heading.to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(heading, run_time=0.7))

        # The tree, dimmed, we only care about one path through it.
        xs = [-6.1 + i * 1.75 for i in range(8)]
        leaf_chips = VGroup(*[chip(leaf_h[i], color=LINE).move_to([x, -0.9, 0])
                              for i, x in enumerate(xs)])
        l1_chips = VGroup(*[
            chip(l1[j], color=LINE).move_to(
                [(xs[2 * j] + xs[2 * j + 1]) / 2, 0.35, 0])
            for j in range(4)])
        l2_chips = VGroup(*[
            chip(l2[k], color=LINE).move_to(
                [(xs[4 * k] + xs[4 * k + 3]) / 2, 1.6, 0])
            for k in range(2)])
        root_chip = chip(root, color=AMBER, scale=0.5).move_to([0, 2.75, 0])
        root_chip.box.set_stroke(AMBER, width=2.5)

        edges = VGroup()
        for i in range(8):
            edges.add(Line(leaf_chips[i].get_top(), l1_chips[i // 2].get_bottom(),
                           stroke_color=LINE, stroke_width=1.2))
        for j in range(4):
            edges.add(Line(l1_chips[j].get_top(), l2_chips[j // 2].get_bottom(),
                           stroke_color=LINE, stroke_width=1.2))
        for k in range(2):
            edges.add(Line(l2_chips[k].get_top(), root_chip.get_bottom(),
                           stroke_color=LINE, stroke_width=1.2))

        self.play(FadeIn(edges), FadeIn(leaf_chips), FadeIn(l1_chips),
                  FadeIn(l2_chips), run_time=0.8)
        self.play(FadeIn(root_chip))

        # Your entry lights up.
        mine = leaf_chips[PROVEN]
        mine_label = Text(LEAVES[PROVEN], font=MONO, color=AMBER).scale(0.36)
        mine_label.next_to(mine, DOWN, buff=0.15)
        self.play(*recolor(mine, AMBER), FadeIn(mine_label))
        self.play(Indicate(mine, color=AMBER, scale_factor=1.2), run_time=0.6)
        self.hold_until(14)

        # The three siblings you're handed, the audit path.
        sib_note = Text("you are handed only the SIBLINGS on the path up",
                        color=MUTED).scale(0.42).move_to([0, -2.05, 0])
        self.play(FadeIn(sib_note))
        siblings = [leaf_chips[3], l1_chips[0], l2_chips[1]]
        for s in siblings:
            self.play(*recolor(s, FG), run_time=0.35)

        # The reader-side climb, one line at a time.
        steps = [
            (f"{short(leaf_h[PROVEN])} + {short(leaf_h[3])}", l1[1]),
            (f"{short(l1[0])} + {short(l1[1])}", l2[0]),
            (f"{short(l2[0])} + {short(l2[1])}", root),
        ]
        lines = VGroup()
        for n, (lhs, out) in enumerate(steps):
            row = Text(f"hash( {lhs} )  →  {short(out)}", font=MONO, color=FG).scale(0.42)
            row.move_to([0, -2.4 - n * 0.42, 0])
            lines.add(row)

        climb_from = [l1_chips[1], l2_chips[0], root_chip]
        self.play(FadeOut(sib_note))
        for n, row in enumerate(lines):
            self.play(FadeIn(row, shift=UP * 0.15), run_time=0.45)
            self.play(*recolor(climb_from[n], AMBER), run_time=0.3)

        verdict = Text("computed root = published root  →  your entry IS in the tree",
                       color=OK).scale(0.46).move_to([0, -3.72, 0])
        self.play(FadeIn(verdict), Indicate(root_chip, color=OK, scale_factor=1.15))
        self.hold_until(32)

        # Now the forgery: same siblings, an entry that was never in the log.
        forged_leaf = h(FORGED)
        f1 = hpair(forged_leaf, leaf_h[3])
        f2 = hpair(l1[0], f1)
        froot = hpair(f2, l2[1])

        self.play(FadeOut(lines), FadeOut(verdict))
        swap = Text(f'now an entry that was never logged: "{FORGED}"',
                    font=MONO, color=DANGER).scale(0.42).move_to([0, -2.05, 0])
        self.play(FadeIn(swap), *recolor(mine, DANGER),
                  mine_label.animate.set_color(DANGER))

        fsteps = [
            (f"{short(forged_leaf)} + {short(leaf_h[3])}", f1),
            (f"{short(l1[0])} + {short(f1)}", f2),
            (f"{short(f2)} + {short(l2[1])}", froot),
        ]
        flines = VGroup()
        for n, (lhs, out) in enumerate(fsteps):
            row = Text(f"hash( {lhs} )  →  {short(out)}", font=MONO, color=DANGER).scale(0.42)
            row.move_to([0, -2.4 - n * 0.42, 0])
            flines.add(row)
        for row in flines:
            self.play(FadeIn(row, shift=UP * 0.15), run_time=0.4)

        fail = Text(f"{short(froot)}  ≠  {short(root)}   →  not in the tree",
                    font=MONO, color=DANGER).scale(0.46).move_to([0, -3.72, 0])
        self.play(FadeIn(fail), *recolor(root_chip, DANGER))
        self.hold_until(44)

        # The scaling number, stated, not described (acceptance criterion).
        self.play(FadeOut(VGroup(edges, leaf_chips, l1_chips, l2_chips, root_chip,
                                 mine_label, swap, flines, fail, heading)))
        scale_head = Text("and it barely grows", color=AMBER).scale(0.55).move_to([0, 2.4, 0])
        rows = VGroup(
            Text("1,000 entries        →   10 hashes", font=MONO, color=FG).scale(0.55),
            Text("1,000,000 entries    →   20 hashes", font=MONO, color=OK).scale(0.55),
            Text("1,000,000,000        →   30 hashes", font=MONO, color=FG).scale(0.55),
        ).arrange(DOWN, buff=0.3, aligned_edge=LEFT)
        note = Text("double the list, add one step", color=MUTED).scale(0.45)
        note.next_to(rows, DOWN, buff=0.6)
        self.play(AddTextLetterByLetter(scale_head, run_time=0.7))
        for r in rows:
            self.play(FadeIn(r, shift=RIGHT * 0.2), run_time=0.45)
        self.play(FadeIn(note))
        self.hold_until(50)
        self.play(FadeOut(VGroup(scale_head, rows, note)))

    # ── Scene 2 (0:50–1:35), the consistency proof ─────────────────────────
    def beat_consistency(self):
        heading = Text("is this the SAME list as last week, only longer?",
                       color=AMBER).scale(0.55).to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(heading, run_time=0.7))

        # Trees drawn as triangles over a strip of leaves, the standard picture.
        def tree(cx, base_w, apex_y, base_y, color, label):
            base_l = [cx - base_w / 2, base_y, 0]
            base_r = [cx + base_w / 2, base_y, 0]
            tri = Polygon([cx, apex_y, 0], base_r, base_l,
                          stroke_color=color, stroke_width=2,
                          fill_color=color, fill_opacity=0.06)
            cap = Text(label, color=color).scale(0.4)
            cap.next_to(tri, DOWN, buff=0.25)
            return VGroup(tri, cap), tri

        old_g, old_tri = tree(-3.6, 4.2, 1.9, -0.6, MUTED, "last week, 40 entries")
        self.play(Create(old_tri), FadeIn(old_g[1]), run_time=0.9)
        self.wait(0.5)

        # Today's tree: wider base, the old one nested in place on the left.
        new_g, new_tri = tree(3.0, 5.4, 2.3, -0.6, AMBER, "today, 49 entries")
        self.play(Create(new_tri), FadeIn(new_g[1]), run_time=0.9)

        inner = Polygon([3.0 - 5.4 / 2 + 4.2 / 2, 1.55, 0],
                        [3.0 - 5.4 / 2 + 4.2, -0.6, 0],
                        [3.0 - 5.4 / 2, -0.6, 0],
                        stroke_color=OK, stroke_width=3.5,
                        fill_color=OK, fill_opacity=0.18)
        added = Polygon([3.0 - 5.4 / 2 + 4.2, -0.6, 0],
                        [3.0 + 5.4 / 2, -0.6, 0],
                        [3.0 + 5.4 / 2 - 0.55, 0.35, 0],
                        stroke_color=FG, stroke_width=1.5,
                        fill_color=FG, fill_opacity=0.06)
        inner_lab = Text("last week's tree, unchanged, in place",
                         color=OK).scale(0.36).move_to([3.0 - 0.35, -1.75, 0])
        added_lab = Text("+9 appended", color=FG).scale(0.34).move_to([5.2, 0.95, 0])

        self.play(Create(inner), FadeIn(inner_lab))
        self.play(Create(added), FadeIn(added_lab))
        proof_note = Text("a consistency proof is the handful of hashes that show this",
                          color=MUTED).scale(0.42).move_to([0, -2.5, 0])
        self.play(FadeIn(proof_note))
        self.hold_until(70)

        # The cheat: edit something already published.
        cheat = Text("if an old entry were edited, say entry #12",
                     color=DANGER).scale(0.45).move_to([0, -3.1, 0])
        self.play(FadeIn(cheat))
        scar = Line([-4.5, -0.6, 0], [-4.5, -0.1, 0], stroke_color=DANGER, stroke_width=5)
        scar2 = Line([1.0, -0.6, 0], [1.0, -0.1, 0], stroke_color=DANGER, stroke_width=5)
        scar_lab = Text("entry #12, edited", color=DANGER).scale(0.32)
        scar_lab.move_to([-4.5, 0.25, 0])
        self.play(Create(scar), Create(scar2), FadeIn(scar_lab), run_time=0.5)
        self.play(old_tri.animate.set_stroke(DANGER).set_fill(DANGER, opacity=0.10),
                  inner.animate.set_stroke(DANGER).set_fill(DANGER, opacity=0.10),
                  inner_lab.animate.set_color(DANGER))
        self.play(Indicate(old_tri, color=DANGER, scale_factor=1.05), run_time=0.6)

        failed = Text("the old tree no longer fits inside the new one",
                      color=DANGER).scale(0.46).move_to([0, -2.5, 0])
        self.play(FadeOut(proof_note), FadeIn(failed))
        gone = Text("NO CONSISTENCY PROOF EXISTS FOR THIS, none can be manufactured",
                    color=DANGER).scale(0.44).move_to([0, -3.1, 0])
        self.play(FadeOut(cheat), FadeIn(gone))
        self.hold_until(86)

        turn = Text('so "append-only" is not a promise. it is something you check.',
                    color=OK).scale(0.5).move_to([0, -3.65, 0])
        self.play(FadeIn(turn))
        self.hold_until(95)
        self.play(FadeOut(VGroup(heading, old_g, new_g, inner, added, inner_lab,
                                 added_lab, failed, gone, turn, scar, scar2,
                                 scar_lab)))

    # ── Scene 3 (1:35–2:05), the signed tree head + the pinned key ─────────
    def beat_sth(self):
        heading = Text("who says which root is real?", color=AMBER).scale(0.55)
        heading.to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(heading, run_time=0.7))

        card = panel(7.4, 2.5, AMBER).move_to([0, 1.35, 0])
        fields = VGroup(
            Text(f"root       {short(LIVE_ROOT, 24)}", font=MONO, color=AMBER).scale(0.42),
            Text(f"tree_size  {LIVE_SIZE}", font=MONO, color=FG).scale(0.42),
            Text(f"timestamp  {LIVE_TS}", font=MONO, color=FG).scale(0.42),
            Text(f"signature  {short(LIVE_SIG, 24)}", font=MONO, color=OK).scale(0.42),
        ).arrange(DOWN, buff=0.18, aligned_edge=LEFT).move_to(card.get_center())
        card_lab = Text("a signed tree head, root, count, time, sealed together",
                        color=MUTED).scale(0.4).next_to(card, DOWN, buff=0.25)
        self.play(Create(card))
        for f in fields:
            self.play(FadeIn(f, shift=RIGHT * 0.15), run_time=0.3)
        self.play(FadeIn(card_lab))
        self.hold_until(110)

        # The pin: the key lives inside the shipped binaries, not on a server.
        key_head = Text("the key we sign with is compiled INTO the client",
                        color=AMBER).scale(0.48).move_to([0, -0.65, 0])
        self.play(FadeIn(key_head))

        def holder(x, name):
            b = panel(5.2, 1.15).move_to([x, -1.9, 0])
            n = Text(name, color=FG).scale(0.38).move_to([x, -1.62, 0])
            k = Text(short(PINNED_KEY, 20), font=MONO, color=OK).scale(0.38)
            k.move_to([x, -2.16, 0])
            return VGroup(b, n, k)

        app = holder(-3.4, "Pollis app (signed binary)")
        cli = holder(3.4, "pollis-verify (CLI)")
        self.play(FadeIn(app), FadeIn(cli))
        not_fetched = Text("not fetched. not configurable.", color=MUTED).scale(0.4)
        not_fetched.move_to([0, -2.85, 0])
        self.play(FadeIn(not_fetched))
        self.wait(1.0)

        # The attack the pin defeats.
        self.play(FadeOut(VGroup(card, fields, card_lab, key_head)))
        attack = Text("a bad server could serve its own key with a matching fake list",
                      color=DANGER).scale(0.46).move_to([0, 1.9, 0])
        served = panel(6.4, 1.5, DANGER).move_to([0, 0.7, 0])
        served_txt = VGroup(
            Text("served public_key.json", color=MUTED).scale(0.36),
            Text("9c41a0d7…  (the server's)", font=MONO, color=DANGER).scale(0.4),
        ).arrange(DOWN, buff=0.14).move_to(served.get_center())
        self.play(FadeIn(attack))
        self.play(Create(served), FadeIn(served_txt))
        arrows = VGroup(
            Line([-1.6, 0.0, 0], [-3.4, -1.3, 0], stroke_color=DANGER, stroke_width=2),
            Line([1.6, 0.0, 0], [3.4, -1.3, 0], stroke_color=DANGER, stroke_width=2),
        )
        self.play(Create(arrows), run_time=0.5)
        reject = Text("served key ≠ pinned key  →  hard ALARM, every time",
                      color=OK).scale(0.5).move_to([0, -3.45, 0])
        self.play(FadeIn(reject),
                  app[0].animate.set_stroke(OK, width=3),
                  cli[0].animate.set_stroke(OK, width=3))
        self.wait(1.4)
        self.hold_until(125)
        self.play(FadeOut(VGroup(heading, app, cli, not_fetched, attack, served,
                                 served_txt, arrows, reject)))

    # ── Scene 4 (2:05–2:57), equivocation, the attack this all exists for ──
    def beat_equivocation(self):
        heading = Text("could different people be shown different lists?", color=AMBER).scale(0.6)
        heading.to_edge(UP).shift(DOWN * 0.1)
        self.play(AddTextLetterByLetter(heading, run_time=0.7))

        server = panel(3.0, 0.85, MUTED).move_to([0, 2.15, 0])
        server_t = Text("the operator", color=MUTED).scale(0.36)
        server_t.move_to(server.get_center())
        self.play(FadeIn(server), FadeIn(server_t))

        def audience(x, who, root_hex, extra):
            b = panel(5.6, 2.9).move_to([x, -0.15, 0])
            title = Text(who, color=FG).scale(0.42).move_to([x, 0.95, 0])
            rt = Text(f"root {short(root_hex, 16)}", font=MONO, color=AMBER).scale(0.4)
            rt.move_to([x, 0.42, 0])
            size = Text(f"tree_size {LIVE_SIZE}", font=MONO, color=MUTED).scale(0.36)
            size.move_to([x, 0.0, 0])
            ex = Text(extra, color=MUTED).scale(0.34).move_to([x, -0.38, 0])
            checks = VGroup(
                Text("append-only   ok", font=MONO, color=OK).scale(0.36),
                Text("signature     ok", font=MONO, color=OK).scale(0.36),
                Text("verifies      ok", font=MONO, color=OK).scale(0.36),
            ).arrange(DOWN, buff=0.12).move_to([x, -1.1, 0])
            return VGroup(b, title, rt, size, ex, checks), checks

        fake_root = h("the log served only to you")
        left, left_checks = audience(-3.6, "what the auditors are served",
                                     LIVE_ROOT, "the real list")
        right, right_checks = audience(3.6, "what you might be sent",
                                       fake_root, "…plus one key that isn't yours")

        line_l = Line([-0.9, 1.75, 0], [-3.6, 1.35, 0], stroke_color=LINE, stroke_width=1.5)
        line_r = Line([0.9, 1.75, 0], [3.6, 1.35, 0], stroke_color=LINE, stroke_width=1.5)
        self.play(Create(line_l), Create(line_r), run_time=0.5)
        self.play(FadeIn(left), FadeIn(right))
        self.play(Indicate(left_checks, color=OK, scale_factor=1.05),
                  Indicate(right_checks, color=OK, scale_factor=1.05), run_time=0.8)

        blind = Text("both append-only. both signed. both verify. "
                     "neither of you can tell alone.",
                     color=DANGER).scale(0.46).move_to([0, -2.3, 0])
        self.play(FadeIn(blind))
        self.hold_until(145)

        # …until they compare.
        self.play(FadeOut(VGroup(server, server_t, line_l, line_r, left, right, blind)))
        compare_head = Text("so compare.", color=AMBER).scale(0.6).move_to([0, 2.3, 0])
        self.play(AddTextLetterByLetter(compare_head, run_time=0.7))

        def head_card(x, who, root_hex, color):
            b = panel(5.6, 2.2, color).move_to([x, 0.45, 0])
            t = Text(who, color=MUTED).scale(0.36).move_to([x, 1.25, 0])
            rows = VGroup(
                Text(f"tree_size  {LIVE_SIZE}", font=MONO, color=FG).scale(0.4),
                Text(f"root       {short(root_hex, 14)}", font=MONO, color=color).scale(0.4),
                Text("signed by  pollis", font=MONO, color=OK).scale(0.4),
            ).arrange(DOWN, buff=0.16, aligned_edge=LEFT).move_to([x, 0.25, 0])
            return VGroup(b, t, rows)

        a = head_card(-3.6, "the head the auditors hold", LIVE_ROOT, AMBER)
        b = head_card(3.6, "the head you hold", fake_root, DANGER)
        self.play(FadeIn(a, shift=RIGHT * 0.2), FadeIn(b, shift=LEFT * 0.2))

        neq = Text("≠", color=DANGER).scale(1.0).move_to([0, 0.45, 0])
        self.play(Write(neq))
        same = Text("same tree_size. different root. both carry a valid signature.",
                    color=FG).scale(0.46).move_to([0, -1.4, 0])
        self.play(FadeIn(same))
        self.hold_until(162)

        stamp = Text("UNDENIABLE", color=DANGER).scale(0.9).move_to([0, -2.3, 0])
        stamp_box = RoundedRectangle(
            corner_radius=0.08, width=stamp.width + 0.6, height=stamp.height + 0.35,
            stroke_color=DANGER, stroke_width=3, fill_opacity=0,
        ).move_to(stamp.get_center())
        self.play(FadeIn(stamp), Create(stamp_box), run_time=0.7)
        self.play(Indicate(VGroup(stamp, stamp_box), color=DANGER, scale_factor=1.08))

        why = Text("this is why it is published, not promised.",
                   color=AMBER).scale(0.5).move_to([0, -3.3, 0])
        self.play(FadeIn(why))
        self.wait(1.6)
        self.hold_until(177)
        self.play(FadeOut(VGroup(compare_head, a, b, neq, same, stamp, stamp_box, why)))

    # ── Scene 5 (2:57–3:05), the chain ─────────────────────────────────────
    def beat_chain(self):
        links = [
            ("root", "fingerprints the list"),
            ("inclusion", "puts your entry in it"),
            ("consistency", "stops us rewriting it"),
            ("signature", "says it is ours"),
            ("comparison", "stops two sets of books"),
        ]
        cards = VGroup()
        for name, sub in links:
            b = panel(2.5, 1.5, AMBER)
            n = Text(name, color=AMBER).scale(0.42)
            s = Text(sub, color=MUTED).scale(0.28)
            txt = VGroup(n, s).arrange(DOWN, buff=0.18).move_to(b.get_center())
            cards.add(VGroup(b, txt))
        cards.arrange(RIGHT, buff=0.28).scale(0.92).move_to([0, 0.4, 0])

        head = Text("each one closes a door", color=FG).scale(0.55).move_to([0, 2.3, 0])
        self.play(AddTextLetterByLetter(head, run_time=0.7))
        for c in cards:
            self.play(FadeIn(c, shift=UP * 0.15), run_time=0.3)
        joins = VGroup(*[
            Line(cards[i].get_right(), cards[i + 1].get_left(),
                 stroke_color=LINE, stroke_width=2)
            for i in range(len(cards) - 1)
        ])
        self.play(Create(joins), run_time=0.6)
        close = Text("nobody has to trust anybody. they just have to compare.",
                     color=AMBER).scale(0.5).move_to([0, -1.9, 0])
        self.play(FadeIn(close))
        self.hold_until(185)
        self.play(FadeOut(VGroup(head, cards, joins, close)))
        self.wait(0.6)
