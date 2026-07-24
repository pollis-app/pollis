"""
Topic 6 — "Identity keys, safety numbers, and TOFU pinning" (#595).

One continuous ~2:45 scene, five beats, NO audio (added later). The attack beat
shows padlocks on BOTH screens while the attack succeeds — that image is the
reason the topic exists. The page then states plainly that safety numbers only
help if you compare, and presents the account-key log as the answer for when
nobody does.

Render (see learn/manim/render.sh):
    learn/manim/render.sh IdentityKeys identity-keys m

Accuracy anchors:
  - .codesight/wiki/safety.md → "Safety number derivation" (60 decimal digits,
    twelve 5-digit blocks; per-user 30-digit fingerprint, order-independent),
    "TOFU pin store", "Key transparency"
  - pollis-core/src/commands/safety.rs → get_safety_number
  - pollis-core/src/commands/transparency.rs → self_audit_account_key,
    audit_peer_account_key
  - docs/transparency.md → "Three domain-separated trees" (account-key tree)
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

# Shape only — a real safety number is 60 decimal digits in twelve 5-digit
# blocks (see .codesight/wiki/safety.md). These stand in for two devices'
# renderings of the same pair of keys, and for the mismatch under attack.
SAFE_MATCH = ("40317  85290  11648", "73025  99164  20873")
SAFE_OTHER = ("72904  16358  40027", "81593  36741  55208")


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


def phone(x, y, who, number, color=OK):
    b = panel(5.4, 2.2, color).move_to([x, y, 0])
    t = VGroup(
        Text(who, color=MUTED).scale(0.34),
        Text("safety number", color=MUTED).scale(0.3),
        Text(number[0], font=MONO, color=color).scale(0.34),
        Text(number[1], font=MONO, color=color).scale(0.34),
    ).arrange(DOWN, buff=0.14).move_to(b.get_center())
    g = VGroup(b, t)
    g.box = b
    return g


class IdentityKeys(Scene):
    def construct(self):
        self.beat_attack()
        self.beat_safety_numbers()
        self.beat_nobody_checks()
        self.beat_log()
        self.beat_tofu()

    def hold_until(self, target):
        """Pad to `target` seconds so each beat stays readable without audio and
        matches learn/manim/scripts/identity-keys.md."""
        now = self.renderer.time
        if target > now:
            self.wait(target - now)

    # ── Scene 1 (0:00–0:35) — the attack, shown working ─────────────────────
    def beat_attack(self):
        head = Text("encryption protects the message. not WHO is on the other end.",
                    color=AMBER).scale(0.5).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        alice = chip("alice", FG, w=2.6, h=0.8, scale=0.34).move_to([-5.0, 1.6, 0])
        server = chip("our server", DANGER, w=3.2, h=0.8, scale=0.34).move_to([0, 1.6, 0])
        bob = chip("bob", FG, w=2.6, h=0.8, scale=0.34).move_to([5.0, 1.6, 0])
        self.play(FadeIn(alice), FadeIn(server), FadeIn(bob))

        ask = Text("alice asks for bob's key…", color=MUTED).scale(0.4)
        ask.move_to([0, 0.6, 0])
        self.play(FadeIn(ask))
        handed = chip('a key labelled "bob"', OK, w=4.6, h=0.7, scale=0.32)
        handed.move_to([-2.6, -0.2, 0])
        self.play(FadeIn(handed))
        self.hold_until(12)

        peel = chip("…actually the server's own key", DANGER, w=6.0, h=0.7, scale=0.32)
        peel.move_to([-2.6, -0.2, 0])
        self.play(FadeOut(handed), FadeIn(peel))

        flow = VGroup(
            Line(alice.box.get_bottom(), server.box.get_bottom() + DOWN * 1.4,
                 stroke_color=DANGER, stroke_width=2),
            Line(server.box.get_bottom() + DOWN * 1.4, bob.box.get_bottom(),
                 stroke_color=DANGER, stroke_width=2),
        )
        opens = chip("opens here · read · re-sealed", DANGER, w=6.4, h=0.7, scale=0.32)
        opens.move_to([0, -1.5, 0])
        self.play(Create(flow), FadeIn(opens))

        locks = VGroup(
            chip("🔒 encrypted", OK, w=3.2, h=0.6, scale=0.3).move_to([-5.0, -2.7, 0]),
            chip("🔒 encrypted", OK, w=3.2, h=0.6, scale=0.3).move_to([5.0, -2.7, 0]),
        )
        self.play(FadeIn(locks))
        both = Text("both screens show a padlock. the whole time.",
                    color=DANGER).scale(0.5).move_to([0, -3.5, 0])
        self.play(FadeIn(both))
        self.hold_until(35)
        self.play(FadeOut(VGroup(head, alice, server, bob, ask, peel, flow, opens,
                                 locks, both)))

    # ── Scene 2 (0:35–1:10) — safety numbers ────────────────────────────────
    def beat_safety_numbers(self):
        head = Text("answer one: safety numbers", color=AMBER).scale(0.6)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        mix = chip("your identity key  +  their identity key", FG, w=8.6, h=0.8, scale=0.34)
        mix.move_to([0, 2.0, 0])
        self.play(FadeIn(mix))

        a = phone(-3.5, 0.4, "on alice's device", SAFE_MATCH)
        b = phone(3.5, 0.4, "on bob's device", SAFE_MATCH)
        self.play(FadeIn(a), FadeIn(b))
        eq = Text("=", color=OK).scale(0.9).move_to([0, 0.4, 0])
        self.play(Write(eq))
        same = Text("same two keys → same number, computed on both devices",
                    color=MUTED).scale(0.4).move_to([0, -1.1, 0])
        self.play(FadeIn(same))
        self.hold_until(52)

        # Under attack the inputs differ, so the numbers differ.
        attacked = Text("now run the attack again: alice is mixing in the SERVER's key",
                        color=DANGER).scale(0.44).move_to([0, -1.1, 0])
        a2 = phone(-3.5, 0.4, "on alice's device", SAFE_OTHER, color=DANGER)
        self.play(FadeOut(same), FadeIn(attacked), FadeOut(a), FadeIn(a2))
        neq = Text("≠", color=DANGER).scale(0.9).move_to([0, 0.4, 0])
        self.play(FadeOut(eq), Write(neq))
        compare = Text("compare them any way we can't interfere with — side by side, "
                       "read aloud, scan the QR", color=FG).scale(0.42)
        compare.move_to([0, -2.1, 0])
        self.play(FadeIn(compare))
        caught = Text("and the middleman is caught immediately.",
                      color=OK).scale(0.5).move_to([0, -3.0, 0])
        self.play(FadeIn(caught))
        self.hold_until(70)
        self.play(FadeOut(VGroup(head, mix, a2, b, neq, attacked, compare, caught)))

    # ── Scene 3 (1:10–1:30) — the honest gap ────────────────────────────────
    def beat_nobody_checks(self):
        head = Text("the catch", color=AMBER).scale(0.6).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        queue = VGroup(*[chip(f"user {i + 1}", MUTED, w=1.9, h=0.6) for i in range(6)])
        queue.arrange(RIGHT, buff=0.35).move_to([0, 1.2, 0])
        self.play(FadeIn(queue))
        gate = chip("compare safety numbers", OK, w=6.0, h=0.8, scale=0.34)
        gate.move_to([0, -0.4, 0])
        self.play(FadeIn(gate))

        # Almost everyone walks straight past.
        self.play(*[q.animate.shift(DOWN * 2.4) for q in queue[:5]], run_time=1.0)
        self.play(queue[5].animate.move_to(gate.get_center() + DOWN * 1.3), run_time=0.6,
                  rate_func=lambda t: t)
        only = Text("it only works if you actually check. most people never do.",
                    color=DANGER).scale(0.5).move_to([0, -2.9, 0])
        self.play(FadeIn(only))
        so = Text("so there's a second answer — one that works even when nobody checks.",
                  color=OK).scale(0.44).move_to([0, -3.5, 0])
        self.play(FadeIn(so))
        self.hold_until(90)
        self.play(FadeOut(VGroup(head, queue, gate, only, so)))

    # ── Scene 4 (1:30–2:15) — the log answer ────────────────────────────────
    def beat_log(self):
        head = Text("answer two: we publish every key, permanently, in public",
                    color=AMBER).scale(0.52).to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        ledger = panel(11.6, 1.6, LINE).move_to([0, 1.4, 0])
        ledger_t = Text("the account-key log — append-only, readable by anyone",
                        color=MUTED).scale(0.34).next_to(ledger, UP, buff=0.15)
        rows = VGroup(*[chip(f"alice v{v}", MUTED, w=2.2, h=0.6) for v in (1, 2, 3)])
        rows.arrange(RIGHT, buff=0.5).move_to([-2.6, 1.4, 0])
        self.play(FadeIn(ledger), FadeIn(ledger_t), FadeIn(rows))

        fake = chip("alice v4 ← the fake", DANGER, w=3.6, h=0.6)
        fake.move_to([3.4, 1.4, 0])
        must = Text("to substitute a key, we would have to publish it HERE",
                    color=DANGER).scale(0.44).move_to([0, 0.1, 0])
        self.play(FadeIn(fake, shift=DOWN * 0.3), FadeIn(must))

        # And it can't be taken back.
        self.play(fake.animate.shift(UP * 0.5).set_opacity(0.6), run_time=0.4)
        self.play(fake.animate.shift(DOWN * 0.5).set_opacity(1.0), run_time=0.4)
        cant = Text("…and then we cannot take it back. the log doesn't work that way.",
                    color=DANGER).scale(0.44).move_to([0, -0.6, 0])
        self.play(FadeIn(cant))
        self.hold_until(115)

        device = chip("alice's own device, reading the log", OK, w=7.4, h=0.8, scale=0.34)
        device.move_to([0, -1.7, 0])
        self.play(FadeIn(device))
        alarm = chip("what the world is shown  ≠  the key I hold   →   ALARM",
                     DANGER, w=9.6, h=0.8, scale=0.34).move_to([0, -2.7, 0])
        self.play(FadeIn(alarm))
        shift = Text("the attack stops being invisible and becomes permanent public evidence.",
                     color=OK).scale(0.46).move_to([0, -3.5, 0])
        self.play(FadeIn(shift))
        self.hold_until(135)
        self.play(FadeOut(VGroup(head, ledger, ledger_t, rows, fake, must, cant,
                                 device, alarm, shift)))

    # ── Scene 5 (2:15–2:45) — TOFU ──────────────────────────────────────────
    def beat_tofu(self):
        head = Text("trust on first use", color=AMBER).scale(0.6)
        head.to_edge(UP).shift(DOWN * 0.1)
        self.play(Write(head))

        first = chip("first conversation → your app pins the key it saw",
                     OK, w=9.4, h=0.8, scale=0.34).move_to([0, 2.0, 0])
        self.play(FadeIn(first))
        later = chip("later: that key CHANGES → you get a banner",
                     AMBER, w=9.4, h=0.8, scale=0.34).move_to([0, 1.0, 0])
        self.play(FadeIn(later))

        innocent = panel(5.8, 1.6, OK).move_to([-3.3, -0.6, 0])
        innocent_t = VGroup(
            Text("a new phone. a reinstall.", color=OK).scale(0.36),
            Text("completely innocent", color=MUTED).scale(0.32),
        ).arrange(DOWN, buff=0.14).move_to(innocent.get_center())
        attack = panel(5.8, 1.6, DANGER).move_to([3.3, -0.6, 0])
        attack_t = VGroup(
            Text("or someone in the middle.", color=DANGER).scale(0.36),
            Text("not innocent at all", color=MUTED).scale(0.32),
        ).arrange(DOWN, buff=0.14).move_to(attack.get_center())
        self.play(FadeIn(innocent), FadeIn(innocent_t))
        self.play(FadeIn(attack), FadeIn(attack_t))

        cannot = Text("your app cannot tell these apart — which is exactly why it asks YOU.",
                      color=FG).scale(0.46).move_to([0, -2.1, 0])
        self.play(FadeIn(cannot))
        close = Text("compare safety numbers once, with the people who matter. "
                     "the log handles the rest.", color=AMBER).scale(0.46)
        close.move_to([0, -3.1, 0])
        self.play(FadeIn(close))
        self.hold_until(165)
        self.play(FadeOut(VGroup(head, first, later, innocent, innocent_t,
                                 attack, attack_t, cannot, close)))
        self.wait(0.5)
