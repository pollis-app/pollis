#!/usr/bin/env python3
"""Generate ``frontend/src/components/Emoji/emojiData.ts``.

What this does
--------------
Enumerates the standard-Unicode emoji repertoire straight out of Python's
``unicodedata`` module and writes it as a typed, dependency-free TypeScript
module for the Discord-style emoji picker. Run it from anywhere:

    python3 scripts/generate-emoji-data.py

The generator is deterministic: given the same ``unicodedata`` build and the
same country table it re-emits byte-identical output, so re-running it on an
unchanged tree produces an empty diff.

Why generated instead of an npm package
---------------------------------------
The obvious alternative is a package like ``emoji-datasource`` or
``unicode-emoji-json``. We do not take one, for two reasons:

1.  Adding a runtime (or even dev) dependency rewrites the frozen
    ``pnpm-lock.yaml``, which drags a lockfile churn + supply-chain review into
    what is otherwise a self-contained UI feature. The emoji picker is not
    worth a new node_modules edge.
2.  The data is *static*. It changes only when Unicode ships a new version,
    which is roughly annual. A checked-in generated file is strictly better
    than a dependency here: it is diffable, auditable, has no install step, and
    the regeneration procedure is this one stdlib-only script.

How category assignment works
-----------------------------
There is no ``emoji-data.txt`` / ``emoji-test.txt`` on the build machine (no
``/usr/share/unicode/``), so the official ``Emoji_Presentation`` property and
the CLDR group names are not available to read. Instead:

*   ``CATEGORY_RANGES`` below hardcodes, per category, the codepoint ranges
    that make up that category. The categories are evaluated in the declared
    order and **the first range that contains a codepoint wins**, so a narrow
    carve-out placed in an earlier category overrides a broad range in a later
    one (e.g. people claims U+1F385 FATHER CHRISTMAS out of the middle of the
    activity block, and food claims U+1F382 BIRTHDAY CAKE). Every emoji
    therefore lands in exactly one category, and the result is de-duplicated.
*   Any codepoint whose ``unicodedata.name()`` raises ``ValueError`` is
    unassigned in this Unicode version and is skipped. That single check is
    what keeps the holes inside each block out of the output, and it is also
    what makes the script forward-compatible: on a newer ``unicodedata`` the
    previously-reserved codepoints inside the declared ranges simply start
    appearing.
*   Display name and search keywords are both ``unicodedata.name()``,
    lowercased with hyphens and underscores folded to spaces
    ("EARTH GLOBE EUROPE-AFRICA" -> "earth globe europe africa"). The emitted
    ``StandardEmoji`` shape has no separate keyword field by design: the name
    *is* the keyword list, which is why the raw Unicode name is kept rather
    than a prettified CLDR short name — "WHITE UP POINTING BACKHAND INDEX"
    matches more useful queries than "backhand index pointing up".

Where the shortcodes come from
-----------------------------
``unicodedata.name()`` is a *name*, not a shortcode: it gives
"face with tears of joy", and nobody types ``:face_with_tears_of_joy:``. The
shortcode people actually type — ``:joy:``, ``:tada:``, ``:100:``, ``:+1:`` —
comes from the gemoji/GitHub set that Slack and Discord both broadly follow,
and no amount of string-munging a Unicode name produces it.

So the aliases are **vendored**, in ``scripts/emoji-shortcodes.json``, for the
same reasons the emoji table itself is generated rather than installed: a pip
or npm dependency would rewrite the frozen lockfile and drag a supply-chain
review into a self-contained UI feature, while a checked-in table is static,
diffable, auditable and has no install step. The JSON carries its own
provenance header (source, pinned version, sha256 of the upstream file, and the
MIT licence it is used under: Copyright (c) 2019 GitHub, Inc.).

Its shape is ``alias -> hyphen-joined uppercase codepoints``, e.g.
``"joy": "1F602"`` and ``"gb": "1F1EC-1F1E7"``, with U+FE0F stripped so the keys
line up with the bare bases this file emits. Aliases are lowercase
``[a-z0-9_+-]``; note that ``+`` and ``-`` are in that set (``:+1:``, ``:-1:``,
``:e-mail:``) and are NOT legal in a *custom* emoji shortcode, which stays
``[a-z0-9_]{2,32}``. Single-character aliases (``:v:``, ``:o:``, ``:x:``) are
kept: they are real, and the two-character minimum before the composer starts
suggesting is a UI rule, not a data rule.

An alias whose target emoji is not in the emitted table (ZWJ sequences,
keycaps, skin-toned variants — none of which this generator enumerates) is
simply dropped, so the emitted set is always a subset of what is renderable.

To refresh the vendored table against a newer gemoji release::

    curl -sSLO https://raw.githubusercontent.com/github/gemoji/<tag>/db/emoji.json
    python3 - <<'PY'
    import json, re
    src = json.load(open("emoji.json"))
    ok = re.compile(r"^[a-z0-9_+-]+$")
    table = {}
    for entry in src:
        char = entry["emoji"].replace("️", "")
        key = "-".join(f"{ord(c):04X}" for c in char)
        for alias in entry["aliases"]:
            if ok.match(alias):
                table[alias] = key
    doc = json.load(open("scripts/emoji-shortcodes.json"))
    doc["shortcodes"] = dict(sorted(table.items()))
    open("scripts/emoji-shortcodes.json", "w").write(
        json.dumps(doc, ensure_ascii=False, indent=2) + "\n")
    PY

then update the ``_version`` / ``_sha256`` fields by hand and re-run this
script.

Known approximation limits
--------------------------
This is a good approximation of the official CLDR groups, not a reproduction
of them. Specifically:

*   The eight categories collapse CLDR's "Smileys & Emotion" and
    "People & Body" into a single ``people`` group, matching how Discord and
    Slack present the picker.
*   Range boundaries are drawn on block runs, so a handful of emoji sit one
    category away from where CLDR files them. Known examples: U+1F4A3 BOMB and
    U+1F4A7 DROPLET fall inside the emotion-symbol run and are filed under
    ``people``; the hand-tool run U+1F6E0..U+1F6E2 falls inside the transport
    run and is filed under ``travel``. These are cosmetic — search is by name
    and is category-independent.
*   Legacy non-emoji pictographs share blocks with real emoji (the U+1F5xx
    range is full of things like "THREE RAYS ABOVE" that no font renders as
    emoji). The ranges are drawn tightly, using singletons through those
    stretches, to keep them out; the current output was audited against V8's
    ``\\p{Emoji}`` property escape and contains zero non-emoji. Widening any
    range risks reintroducing them, so re-run that audit if you do.
*   ``char`` is the bare base codepoint, with **no** U+FE0F variation selector.
    Roughly 190 of these entries (the ones from the older BMP symbol blocks,
    e.g. U+261D, U+2328, U+2764) default to *text* presentation, so a renderer
    that wants a colour glyph should append U+FE0F at display time. The
    selector is deliberately not baked into the data because it would make the
    bases multi-codepoint and break the skin-tone composition contract that
    ``applySkinTone`` documents.
*   ZWJ sequences (family groupings, profession sequences, flag-tag sequences
    such as England/Scotland/Wales) are **not** enumerated. Only single
    codepoints plus the regional-indicator flag pairs are emitted.
*   Skin-tone variants are not enumerated either; the ``tonable`` flag marks
    the bases that accept a Fitzpatrick modifier and the runtime composes the
    variant. See ``EMOJI_MODIFIER_BASE_RANGES``.
"""

from __future__ import annotations

import json
import unicodedata
from pathlib import Path

# Repository root, derived from this file's location (scripts/..).
REPO_ROOT = Path(__file__).resolve().parent.parent

OUTPUT_PATH = REPO_ROOT / "frontend" / "src" / "components" / "Emoji" / "emojiData.ts"

# The mobile app carries an identical copy (it cannot import across the
# frontend/mobile boundary — mobile is a standalone Expo project). Both are
# written on every run so they cannot drift.
MOBILE_OUTPUT_PATH = REPO_ROOT / "mobile" / "components" / "emoji" / "emojiData.ts"

# The vendored gemoji alias table. See "Where the shortcodes come from" above
# for its provenance, licence and refresh recipe.
SHORTCODES_PATH = REPO_ROOT / "scripts" / "emoji-shortcodes.json"

# Category ids and their human-readable labels, in picker order.
CATEGORY_LABELS: list[tuple[str, str]] = [
    ("people", "People"),
    ("nature", "Nature"),
    ("food", "Food"),
    ("activity", "Activities"),
    ("travel", "Travel"),
    ("objects", "Objects"),
    ("symbols", "Symbols"),
    ("flags", "Flags"),
]

# Inclusive codepoint ranges per category. Evaluated in this order; the first
# range containing a codepoint claims it, so earlier categories can carve
# single codepoints out of a later category's broad block range.
CATEGORY_RANGES: dict[str, list[tuple[int, int]]] = {
    # CLDR "Smileys & Emotion" + "People & Body", including the person-sport
    # and person-activity emoji that CLDR files under People rather than
    # Activities (they are the ones that take skin tones).
    "people": [
        (0x261D, 0x261D),
        (0x2639, 0x263A),
        (0x26F9, 0x26F9),
        (0x270A, 0x270D),
        (0x2763, 0x2764),
        (0x1F385, 0x1F385),
        (0x1F3C2, 0x1F3C4),
        (0x1F3C7, 0x1F3C7),
        (0x1F3CA, 0x1F3CC),
        (0x1F440, 0x1F450),
        (0x1F463, 0x1F487),
        (0x1F48B, 0x1F48C),
        (0x1F48F, 0x1F48F),
        (0x1F491, 0x1F491),
        (0x1F493, 0x1F49F),
        (0x1F4A2, 0x1F4AD),
        (0x1F574, 0x1F575),
        (0x1F57A, 0x1F57A),
        (0x1F590, 0x1F590),
        (0x1F595, 0x1F596),
        (0x1F5A4, 0x1F5A4),
        (0x1F5E3, 0x1F5E3),
        (0x1F5E8, 0x1F5E8),
        (0x1F5EF, 0x1F5EF),
        (0x1F600, 0x1F64F),
        (0x1F6A3, 0x1F6A3),
        (0x1F6B4, 0x1F6B6),
        (0x1F6C0, 0x1F6C0),
        (0x1F6CC, 0x1F6CC),
        (0x1F90C, 0x1F90F),
        (0x1F918, 0x1F939),
        (0x1F93C, 0x1F93E),
        (0x1F970, 0x1F97A),
        (0x1F9B0, 0x1F9B9),
        (0x1F9BB, 0x1F9BB),
        (0x1F9BE, 0x1F9BF),
        (0x1F9CD, 0x1F9E1),
        (0x1FAC0, 0x1FAC5),
        (0x1FAF0, 0x1FAF8),
    ],
    # CLDR "Animals & Nature" (plants and animals; sky & weather is filed
    # under Travel & Places, matching CLDR).
    "nature": [
        (0x1F331, 0x1F335),
        (0x1F337, 0x1F33C),
        (0x1F33E, 0x1F343),
        (0x1F400, 0x1F43F),
        (0x1F490, 0x1F490),
        (0x1F4AE, 0x1F4AE),
        (0x1F577, 0x1F578),
        (0x1F980, 0x1F9AE),
        (0x1FAA8, 0x1FAA8),
        (0x1FAB0, 0x1FABF),
    ],
    # CLDR "Food & Drink", including dishware.
    "food": [
        (0x2615, 0x2615),
        (0x1F32D, 0x1F330),
        (0x1F336, 0x1F336),
        (0x1F33D, 0x1F33D),
        (0x1F344, 0x1F37F),
        (0x1F382, 0x1F382),
        (0x1F942, 0x1F944),
        (0x1F950, 0x1F96F),
        (0x1F9C0, 0x1F9CB),
        (0x1FAD0, 0x1FADC),
    ],
    # CLDR "Activities": events, games, sport equipment, awards, arts.
    "activity": [
        (0x265F, 0x265F),
        (0x26BD, 0x26BE),
        (0x26F3, 0x26F3),
        (0x26F7, 0x26F8),
        (0x1F004, 0x1F004),
        (0x1F0CF, 0x1F0CF),
        (0x1F380, 0x1F393),
        (0x1F396, 0x1F397),
        (0x1F3A3, 0x1F3A3),
        (0x1F3A8, 0x1F3A8),
        (0x1F3AA, 0x1F3AB),
        (0x1F3AD, 0x1F3B4),
        (0x1F3BD, 0x1F3C1),
        (0x1F3C5, 0x1F3C6),
        (0x1F3C8, 0x1F3C9),
        (0x1F3CF, 0x1F3D3),
        (0x1F3F5, 0x1F3F5),
        (0x1F3F8, 0x1F3F9),
        (0x1F52E, 0x1F52E),
        (0x1F579, 0x1F579),
        (0x1F6DD, 0x1F6DD),
        (0x1F93A, 0x1F93A),
        (0x1F945, 0x1F945),
        (0x1F947, 0x1F94C),
        (0x1F9E7, 0x1F9E9),
        (0x1FA80, 0x1FA86),
    ],
    # CLDR "Travel & Places", which also owns sky & weather and time.
    "travel": [
        (0x231A, 0x231B),
        (0x23F0, 0x23F3),
        (0x2600, 0x2604),
        (0x2614, 0x2614),
        (0x26C4, 0x26C5),
        (0x26C8, 0x26C8),
        (0x26E9, 0x26EA),
        (0x26F0, 0x26F5),
        (0x26FA, 0x26FA),
        (0x26FD, 0x26FD),
        (0x2708, 0x2708),
        (0x1F300, 0x1F321),
        (0x1F324, 0x1F32C),
        (0x1F3A0, 0x1F3A2),
        (0x1F3CD, 0x1F3CE),
        (0x1F3D4, 0x1F3F0),
        (0x1F492, 0x1F492),
        (0x1F4BA, 0x1F4BA),
        (0x1F550, 0x1F567),
        (0x1F570, 0x1F570),
        (0x1F5FA, 0x1F5FF),
        (0x1F680, 0x1F6A9),
        (0x1F6AB, 0x1F6B3),
        (0x1F6B7, 0x1F6B8),
        (0x1F6D1, 0x1F6D1),
        (0x1F6D5, 0x1F6D6),
        (0x1F6DE, 0x1F6DF),
        (0x1F6E0, 0x1F6E5),
        (0x1F6E9, 0x1F6E9),
        (0x1F6EB, 0x1F6EC),
        (0x1F6F0, 0x1F6F0),
        (0x1F6F3, 0x1F6FC),
        (0x1F9F3, 0x1F9F3),
        (0x1FA90, 0x1FA90),
    ],
    # CLDR "Objects": clothing, tools, office, household, science, music.
    "objects": [
        (0x260E, 0x260E),
        (0x2328, 0x2328),
        (0x2692, 0x2692),
        (0x2694, 0x2694),
        (0x2696, 0x2697),
        (0x2699, 0x2699),
        (0x26B0, 0x26B1),
        (0x26CF, 0x26CF),
        (0x26D1, 0x26D1),
        (0x26D3, 0x26D3),
        (0x2702, 0x2702),
        (0x2709, 0x2709),
        (0x270F, 0x270F),
        (0x2712, 0x2712),
        (0x1F399, 0x1F39B),
        (0x1F39E, 0x1F39F),
        (0x1F3A4, 0x1F3A7),
        (0x1F3A9, 0x1F3A9),
        (0x1F3AC, 0x1F3AC),
        (0x1F3B5, 0x1F3BC),
        (0x1F3F7, 0x1F3F7),
        (0x1F3FA, 0x1F3FA),
        (0x1F451, 0x1F462),
        (0x1F488, 0x1F48A),
        (0x1F48D, 0x1F48E),
        (0x1F4A1, 0x1F4A1),
        (0x1F4B0, 0x1F4B9),
        (0x1F4BB, 0x1F4FD),
        (0x1F4FF, 0x1F4FF),
        (0x1F525, 0x1F52D),
        (0x1F56F, 0x1F56F),
        (0x1F576, 0x1F576),
        (0x1F587, 0x1F587),
        (0x1F58A, 0x1F58D),
        (0x1F5A5, 0x1F5A5),
        (0x1F5A8, 0x1F5A8),
        (0x1F5B1, 0x1F5B2),
        (0x1F5BC, 0x1F5BC),
        (0x1F5C2, 0x1F5C4),
        (0x1F5D1, 0x1F5D3),
        (0x1F5DC, 0x1F5DE),
        (0x1F5E1, 0x1F5E1),
        (0x1F5F3, 0x1F5F3),
        (0x1F6AA, 0x1F6AA),
        (0x1F6BD, 0x1F6BD),
        (0x1F6BF, 0x1F6BF),
        (0x1F6C1, 0x1F6C1),
        (0x1F6D2, 0x1F6D2),
        (0x1F6D7, 0x1F6D7),
        (0x1F6DC, 0x1F6DC),
        (0x1F97B, 0x1F97F),
        (0x1F9AF, 0x1F9AF),
        (0x1F9BC, 0x1F9BD),
        (0x1F9E2, 0x1F9E6),
        (0x1F9EA, 0x1F9FF),
        (0x1FA70, 0x1FA7F),
        (0x1FA87, 0x1FA89),
        (0x1FA91, 0x1FAAF),
    ],
    # CLDR "Symbols": signage, arrows, geometric shapes, keycaps, alphanumerics.
    "symbols": [
        (0x00A9, 0x00A9),
        (0x00AE, 0x00AE),
        (0x203C, 0x203C),
        (0x2049, 0x2049),
        (0x2122, 0x2122),
        (0x2139, 0x2139),
        (0x2194, 0x2199),
        (0x21A9, 0x21AA),
        (0x23CF, 0x23CF),
        (0x23E9, 0x23EF),
        (0x23F8, 0x23FA),
        (0x24C2, 0x24C2),
        (0x25AA, 0x25AB),
        (0x25B6, 0x25B6),
        (0x25C0, 0x25C0),
        (0x25FB, 0x25FE),
        (0x2611, 0x2611),
        (0x2626, 0x2626),
        (0x262A, 0x262A),
        (0x262E, 0x262F),
        (0x2638, 0x2638),
        (0x2648, 0x2653),
        (0x267B, 0x267B),
        (0x267E, 0x267F),
        (0x2695, 0x2695),
        (0x269B, 0x269C),
        (0x26A0, 0x26A1),
        (0x26A7, 0x26A7),
        (0x26AA, 0x26AB),
        (0x26CE, 0x26CE),
        (0x26D4, 0x26D4),
        (0x2705, 0x2705),
        (0x2714, 0x2714),
        (0x2716, 0x2716),
        (0x271D, 0x271D),
        (0x2721, 0x2721),
        (0x2728, 0x2728),
        (0x2733, 0x2734),
        (0x2744, 0x2744),
        (0x2747, 0x2747),
        (0x274C, 0x274C),
        (0x274E, 0x274E),
        (0x2753, 0x2755),
        (0x2757, 0x2757),
        (0x2795, 0x2797),
        (0x27A1, 0x27A1),
        (0x27B0, 0x27B0),
        (0x27BF, 0x27BF),
        (0x2934, 0x2935),
        (0x2B05, 0x2B07),
        (0x2B1B, 0x2B1C),
        (0x2B50, 0x2B50),
        (0x2B55, 0x2B55),
        (0x3030, 0x3030),
        (0x303D, 0x303D),
        (0x3297, 0x3297),
        (0x3299, 0x3299),
        (0x1F170, 0x1F171),
        (0x1F17E, 0x1F17F),
        (0x1F18E, 0x1F18E),
        (0x1F191, 0x1F19A),
        (0x1F201, 0x1F202),
        (0x1F21A, 0x1F21A),
        (0x1F22F, 0x1F22F),
        (0x1F232, 0x1F23A),
        (0x1F250, 0x1F251),
        (0x1F4A0, 0x1F4A0),
        (0x1F4AF, 0x1F4AF),
        (0x1F500, 0x1F524),
        (0x1F52F, 0x1F53D),
        (0x1F6B9, 0x1F6BC),
        (0x1F6BE, 0x1F6BE),
        (0x1F6C2, 0x1F6C5),
        (0x1F6D0, 0x1F6D0),
        (0x1F7E0, 0x1F7EB),
        (0x1F7F0, 0x1F7F0),
    ],
    # Built separately from regional-indicator pairs; see build_flags().
    "flags": [],
}

# Emoji_Modifier_Base: the bases that accept a Fitzpatrick skin-tone modifier.
# Hardcoded because the derived property file is not on disk. Filtered at
# generation time down to the codepoints that actually made it into the set.
EMOJI_MODIFIER_BASE_RANGES: list[tuple[int, int]] = [
    (0x261D, 0x261D),
    (0x26F9, 0x26F9),
    (0x270A, 0x270D),
    (0x1F385, 0x1F385),
    (0x1F3C2, 0x1F3C4),
    (0x1F3C7, 0x1F3C7),
    (0x1F3CA, 0x1F3CC),
    (0x1F442, 0x1F443),
    (0x1F446, 0x1F450),
    (0x1F466, 0x1F478),
    (0x1F47C, 0x1F47C),
    (0x1F481, 0x1F483),
    (0x1F485, 0x1F487),
    (0x1F48F, 0x1F48F),
    (0x1F491, 0x1F491),
    (0x1F4AA, 0x1F4AA),
    (0x1F574, 0x1F575),
    (0x1F57A, 0x1F57A),
    (0x1F590, 0x1F590),
    (0x1F595, 0x1F596),
    (0x1F645, 0x1F647),
    (0x1F64B, 0x1F64F),
    (0x1F6A3, 0x1F6A3),
    (0x1F6B4, 0x1F6B6),
    (0x1F6C0, 0x1F6C0),
    (0x1F6CC, 0x1F6CC),
    (0x1F90C, 0x1F90C),
    (0x1F90F, 0x1F90F),
    (0x1F918, 0x1F91F),
    (0x1F926, 0x1F926),
    (0x1F930, 0x1F939),
    (0x1F93C, 0x1F93E),
    (0x1F977, 0x1F977),
    (0x1F9B5, 0x1F9B6),
    (0x1F9B8, 0x1F9B9),
    (0x1F9BB, 0x1F9BB),
    (0x1F9CD, 0x1F9DF),
    (0x1FAC3, 0x1FAC5),
    (0x1FAF0, 0x1FAF8),
]

# The five Fitzpatrick modifiers, in Discord's order. Index 0 is "no tone".
SKIN_TONES = ["", "\U0001F3FB", "\U0001F3FC", "\U0001F3FD", "\U0001F3FE", "\U0001F3FF"]

# Regional indicator A, the base of the flag pair encoding.
REGIONAL_INDICATOR_A = 0x1F1E6

# Country tables the generator prefers, in order, before falling back.
ZONEINFO_TABLE = Path("/usr/share/zoneinfo/iso3166.tab")
ISO_CODES_JSON = Path("/usr/share/iso-codes/json/iso_3166-1.json")

# ISO 3166-1 alpha-2 fallback, used only when neither system table is present.
# Names follow the tzdata short-form convention.
FALLBACK_COUNTRY_NAMES: dict[str, str] = {
    "AD": "Andorra", "AE": "United Arab Emirates", "AF": "Afghanistan",
    "AG": "Antigua & Barbuda", "AI": "Anguilla", "AL": "Albania", "AM": "Armenia",
    "AO": "Angola", "AQ": "Antarctica", "AR": "Argentina", "AS": "Samoa (American)",
    "AT": "Austria", "AU": "Australia", "AW": "Aruba", "AX": "Åland Islands",
    "AZ": "Azerbaijan", "BA": "Bosnia & Herzegovina", "BB": "Barbados", "BD": "Bangladesh",
    "BE": "Belgium", "BF": "Burkina Faso", "BG": "Bulgaria", "BH": "Bahrain", "BI": "Burundi",
    "BJ": "Benin", "BL": "St Barthelemy", "BM": "Bermuda", "BN": "Brunei", "BO": "Bolivia",
    "BQ": "Caribbean NL", "BR": "Brazil", "BS": "Bahamas", "BT": "Bhutan",
    "BV": "Bouvet Island", "BW": "Botswana", "BY": "Belarus", "BZ": "Belize", "CA": "Canada",
    "CC": "Cocos (Keeling) Islands", "CD": "Congo (Dem. Rep.)", "CF": "Central African Rep.",
    "CG": "Congo (Rep.)", "CH": "Switzerland", "CI": "Côte d’Ivoire", "CK": "Cook Islands",
    "CL": "Chile", "CM": "Cameroon", "CN": "China", "CO": "Colombia", "CR": "Costa Rica",
    "CU": "Cuba", "CV": "Cape Verde", "CW": "Curaçao", "CX": "Christmas Island",
    "CY": "Cyprus", "CZ": "Czech Republic", "DE": "Germany", "DJ": "Djibouti", "DK": "Denmark",
    "DM": "Dominica", "DO": "Dominican Republic", "DZ": "Algeria", "EC": "Ecuador",
    "EE": "Estonia", "EG": "Egypt", "EH": "Western Sahara", "ER": "Eritrea", "ES": "Spain",
    "ET": "Ethiopia", "FI": "Finland", "FJ": "Fiji", "FK": "Falkland Islands",
    "FM": "Micronesia", "FO": "Faroe Islands", "FR": "France", "GA": "Gabon",
    "GB": "Britain (UK)", "GD": "Grenada", "GE": "Georgia", "GF": "French Guiana",
    "GG": "Guernsey", "GH": "Ghana", "GI": "Gibraltar", "GL": "Greenland", "GM": "Gambia",
    "GN": "Guinea", "GP": "Guadeloupe", "GQ": "Equatorial Guinea", "GR": "Greece",
    "GS": "South Georgia & the South Sandwich Islands", "GT": "Guatemala", "GU": "Guam",
    "GW": "Guinea-Bissau", "GY": "Guyana", "HK": "Hong Kong",
    "HM": "Heard Island & McDonald Islands", "HN": "Honduras", "HR": "Croatia", "HT": "Haiti",
    "HU": "Hungary", "ID": "Indonesia", "IE": "Ireland", "IL": "Israel", "IM": "Isle of Man",
    "IN": "India", "IO": "British Indian Ocean Territory", "IQ": "Iraq", "IR": "Iran",
    "IS": "Iceland", "IT": "Italy", "JE": "Jersey", "JM": "Jamaica", "JO": "Jordan",
    "JP": "Japan", "KE": "Kenya", "KG": "Kyrgyzstan", "KH": "Cambodia", "KI": "Kiribati",
    "KM": "Comoros", "KN": "St Kitts & Nevis", "KP": "Korea (North)", "KR": "Korea (South)",
    "KW": "Kuwait", "KY": "Cayman Islands", "KZ": "Kazakhstan", "LA": "Laos", "LB": "Lebanon",
    "LC": "St Lucia", "LI": "Liechtenstein", "LK": "Sri Lanka", "LR": "Liberia",
    "LS": "Lesotho", "LT": "Lithuania", "LU": "Luxembourg", "LV": "Latvia", "LY": "Libya",
    "MA": "Morocco", "MC": "Monaco", "MD": "Moldova", "ME": "Montenegro",
    "MF": "St Martin (French)", "MG": "Madagascar", "MH": "Marshall Islands",
    "MK": "North Macedonia", "ML": "Mali", "MM": "Myanmar (Burma)", "MN": "Mongolia",
    "MO": "Macau", "MP": "Northern Mariana Islands", "MQ": "Martinique", "MR": "Mauritania",
    "MS": "Montserrat", "MT": "Malta", "MU": "Mauritius", "MV": "Maldives", "MW": "Malawi",
    "MX": "Mexico", "MY": "Malaysia", "MZ": "Mozambique", "NA": "Namibia",
    "NC": "New Caledonia", "NE": "Niger", "NF": "Norfolk Island", "NG": "Nigeria",
    "NI": "Nicaragua", "NL": "Netherlands", "NO": "Norway", "NP": "Nepal", "NR": "Nauru",
    "NU": "Niue", "NZ": "New Zealand", "OM": "Oman", "PA": "Panama", "PE": "Peru",
    "PF": "French Polynesia", "PG": "Papua New Guinea", "PH": "Philippines", "PK": "Pakistan",
    "PL": "Poland", "PM": "St Pierre & Miquelon", "PN": "Pitcairn", "PR": "Puerto Rico",
    "PS": "Palestine", "PT": "Portugal", "PW": "Palau", "PY": "Paraguay", "QA": "Qatar",
    "RE": "Réunion", "RO": "Romania", "RS": "Serbia", "RU": "Russia", "RW": "Rwanda",
    "SA": "Saudi Arabia", "SB": "Solomon Islands", "SC": "Seychelles", "SD": "Sudan",
    "SE": "Sweden", "SG": "Singapore", "SH": "St Helena", "SI": "Slovenia",
    "SJ": "Svalbard & Jan Mayen", "SK": "Slovakia", "SL": "Sierra Leone", "SM": "San Marino",
    "SN": "Senegal", "SO": "Somalia", "SR": "Suriname", "SS": "South Sudan",
    "ST": "Sao Tome & Principe", "SV": "El Salvador", "SX": "St Maarten (Dutch)",
    "SY": "Syria", "SZ": "Eswatini (Swaziland)", "TC": "Turks & Caicos Is", "TD": "Chad",
    "TF": "French S. Terr.", "TG": "Togo", "TH": "Thailand", "TJ": "Tajikistan",
    "TK": "Tokelau", "TL": "East Timor", "TM": "Turkmenistan", "TN": "Tunisia", "TO": "Tonga",
    "TR": "Turkey", "TT": "Trinidad & Tobago", "TV": "Tuvalu", "TW": "Taiwan",
    "TZ": "Tanzania", "UA": "Ukraine", "UG": "Uganda", "UM": "US minor outlying islands",
    "US": "United States", "UY": "Uruguay", "UZ": "Uzbekistan", "VA": "Vatican City",
    "VC": "St Vincent", "VE": "Venezuela", "VG": "Virgin Islands (UK)",
    "VI": "Virgin Islands (US)", "VN": "Vietnam", "VU": "Vanuatu", "WF": "Wallis & Futuna",
    "WS": "Samoa (western)", "YE": "Yemen", "YT": "Mayotte", "ZA": "South Africa",
    "ZM": "Zambia", "ZW": "Zimbabwe",
}


def expand(ranges: list[tuple[int, int]]) -> set[int]:
    """Flatten inclusive ``(start, end)`` ranges into a set of codepoints."""
    out: set[int] = set()
    for start, end in ranges:
        out.update(range(start, end + 1))
    return out


def display_name(codepoint: int) -> str | None:
    """Unicode name, lowercased and normalised, or None if unassigned."""
    try:
        raw = unicodedata.name(chr(codepoint))
    except ValueError:
        return None
    return raw.lower().replace("-", " ").replace("_", " ")


def load_shortcodes() -> tuple[dict[str, list[str]], str]:
    """Invert the vendored alias table into ``char -> sorted aliases``.

    Aliases are sorted rather than kept in gemoji's own order so the output is
    a pure function of the checked-in JSON: two aliases for one emoji must not
    swap places because upstream reordered a list.
    """
    payload = json.loads(SHORTCODES_PATH.read_text(encoding="utf-8"))
    by_char: dict[str, list[str]] = {}
    for alias, key in payload["shortcodes"].items():
        char = "".join(chr(int(part, 16)) for part in key.split("-"))
        by_char.setdefault(char, []).append(alias)
    for aliases in by_char.values():
        aliases.sort()
    return by_char, str(payload["_version"])


def read_zoneinfo_table(path: Path) -> dict[str, str]:
    """Parse tzdata's iso3166.tab: ``XX<TAB>Country Name``, ``#`` comments."""
    names: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        code, _, name = line.partition("\t")
        code = code.strip()
        name = name.strip()
        if len(code) == 2 and code.isalpha() and name:
            names[code.upper()] = name
    return names


def read_iso_codes_json(path: Path) -> dict[str, str]:
    """Parse the iso-codes package's iso_3166-1.json."""
    payload = json.loads(path.read_text(encoding="utf-8"))
    names: dict[str, str] = {}
    for entry in payload.get("3166-1", []):
        code = entry.get("alpha_2", "")
        name = entry.get("name", "")
        if len(code) == 2 and name:
            names[code.upper()] = name
    return names


def load_country_names() -> tuple[dict[str, str], str]:
    """Return the ISO 3166-1 alpha-2 table plus the source it came from."""
    if ZONEINFO_TABLE.is_file():
        names = read_zoneinfo_table(ZONEINFO_TABLE)
        if names:
            return names, str(ZONEINFO_TABLE)
    if ISO_CODES_JSON.is_file():
        names = read_iso_codes_json(ISO_CODES_JSON)
        if names:
            return names, str(ISO_CODES_JSON)
    return dict(FALLBACK_COUNTRY_NAMES), "hardcoded fallback table"


def flag_char(code: str) -> str:
    """Turn an alpha-2 code into its regional-indicator pair."""
    return "".join(
        chr(REGIONAL_INDICATOR_A + (ord(letter) - ord("A"))) for letter in code
    )


def build_entries() -> tuple[list[dict[str, object]], dict[str, int], str, str]:
    """Assemble every entry, in category order then codepoint order."""
    tonable_bases = expand(EMOJI_MODIFIER_BASE_RANGES)
    shortcodes, shortcode_version = load_shortcodes()
    entries: list[dict[str, object]] = []
    counts: dict[str, int] = {}
    claimed: set[int] = set()

    for category_id, _label in CATEGORY_LABELS:
        if category_id == "flags":
            continue
        count = 0
        for codepoint in sorted(expand(CATEGORY_RANGES[category_id])):
            # First matching category wins, so a codepoint already taken by an
            # earlier category is skipped here.
            if codepoint in claimed:
                continue
            name = display_name(codepoint)
            # Unassigned in this Unicode version: this is the hole filter.
            if name is None:
                continue
            claimed.add(codepoint)
            entries.append(
                {
                    "char": chr(codepoint),
                    "name": name,
                    "category": category_id,
                    "tonable": codepoint in tonable_bases,
                    "shortcodes": shortcodes.get(chr(codepoint), []),
                }
            )
            count += 1
        counts[category_id] = count

    country_names, source = load_country_names()
    flag_count = 0
    for code in sorted(country_names):
        # "flag" is appended because StandardEmoji has no separate keyword
        # field: the name doubles as the search index.
        entries.append(
            {
                "char": flag_char(code),
                "name": f"{country_names[code].lower()} flag",
                "category": "flags",
                "tonable": False,
                "shortcodes": shortcodes.get(flag_char(code), []),
            }
        )
        flag_count += 1
    counts["flags"] = flag_count

    return entries, counts, source, shortcode_version


def ts_string(value: str) -> str:
    """Quote a value as a double-quoted TypeScript string literal."""
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def render(
    entries: list[dict[str, object]],
    counts: dict[str, int],
    source: str,
    shortcode_version: str,
) -> str:
    """Render the whole TypeScript module."""
    total = len(entries)
    summary = ", ".join(f"{cid} {counts[cid]}" for cid, _label in CATEGORY_LABELS)
    with_shortcodes = sum(1 for entry in entries if entry["shortcodes"])
    alias_total = sum(len(entry["shortcodes"]) for entry in entries)

    lines: list[str] = []
    lines.append("// GENERATED FILE — DO NOT EDIT BY HAND.")
    lines.append("//")
    lines.append("// Produced by `scripts/generate-emoji-data.py`. To regenerate:")
    lines.append("//")
    lines.append("//     python3 scripts/generate-emoji-data.py")
    lines.append("//")
    lines.append(f"// Unicode data version: {unicodedata.unidata_version}")
    lines.append(f"// Country names from:   {source}")
    lines.append(f"// Shortcodes from:      scripts/emoji-shortcodes.json ({shortcode_version})")
    lines.append(f"// Entries:              {total} ({summary})")
    lines.append(f"// With shortcodes:      {with_shortcodes} entries, {alias_total} aliases")
    lines.append("//")
    lines.append("// Names come straight from `unicodedata.name()`, lowercased, and double as")
    lines.append("// the search keywords — there is deliberately no separate keyword field.")
    lines.append("// Categories approximate the CLDR groups; see the generator's docstring for")
    lines.append("// the exact ranges and the known approximation limits.")
    lines.append("//")
    lines.append("// `char` is the bare base codepoint with no U+FE0F variation selector. Entries")
    lines.append("// drawn from the older BMP symbol blocks default to text presentation, so a")
    lines.append("// renderer wanting a colour glyph should append U+FE0F at display time.")
    lines.append("//")
    lines.append("// `shortcodes` are the `:joy:`-style aliases, vendored from gemoji (MIT) — see")
    lines.append("// the generator's \"Where the shortcodes come from\" section. They may contain")
    lines.append("// `+` and `-`, which a CUSTOM emoji shortcode (`[a-z0-9_]{2,32}`) may not.")
    lines.append("")
    lines.append("export type EmojiCategoryId =")
    for index, (category_id, _label) in enumerate(CATEGORY_LABELS):
        suffix = ";" if index == len(CATEGORY_LABELS) - 1 else ""
        lines.append(f"  | {ts_string(category_id)}{suffix}")
    lines.append("")
    lines.append("export interface EmojiCategory {")
    lines.append("  readonly id: EmojiCategoryId;")
    lines.append("  readonly label: string;")
    lines.append("}")
    lines.append("")
    lines.append("export interface StandardEmoji {")
    lines.append("  /** The emoji character itself. */")
    lines.append("  readonly char: string;")
    lines.append('  /** Human-readable name, lowercase (e.g. "grinning face"). */')
    lines.append("  readonly name: string;")
    lines.append("  readonly category: EmojiCategoryId;")
    lines.append("  /** True when this base emoji accepts a Fitzpatrick skin-tone modifier. */")
    lines.append("  readonly tonable: boolean;")
    lines.append("  /**")
    lines.append('   * `:shortcode:` aliases, e.g. ["+1", "thumbsup"]. Often empty — only the')
    lines.append("   * emoji gemoji names have one. Every alias is unique across the table.")
    lines.append("   */")
    lines.append("  readonly shortcodes: readonly string[];")
    lines.append("}")
    lines.append("")
    lines.append("export const EMOJI_CATEGORIES: readonly EmojiCategory[] = [")
    for category_id, label in CATEGORY_LABELS:
        lines.append(f"  {{ id: {ts_string(category_id)}, label: {ts_string(label)} }},")
    lines.append("];")
    lines.append("")
    lines.append("/** The five Fitzpatrick modifiers, in Discord's order (index 0 = default/none). */")
    tone_literals = ", ".join(
        '""' if tone == "" else f'"\\u{{{ord(tone):X}}}"' for tone in SKIN_TONES
    )
    lines.append(f"export const SKIN_TONES: readonly string[] = [{tone_literals}];")
    lines.append("")
    lines.append("// The table is emitted as positional rows rather than object literals: at this")
    lines.append("// size a literal array of 1500+ distinct object types makes tsc give up with")
    lines.append("// \"union type that is too complex to represent\" (TS2590). Rows are contextually")
    lines.append("// typed against the single EmojiRow tuple type, so there is no union to widen,")
    lines.append("// and the file stays one entry per line for readable diffs.")
    lines.append("type EmojiRow = readonly [")
    lines.append("  char: string,")
    lines.append("  name: string,")
    lines.append("  category: EmojiCategoryId,")
    lines.append("  tonable: 0 | 1,")
    lines.append("  // Space-joined rather than a nested array literal: 1500+ inline arrays cost")
    lines.append("  // far more bytes than they buy, and no shortcode can contain a space.")
    lines.append("  shortcodes: string,")
    lines.append("];")
    lines.append("")
    lines.append("const EMOJI_ROWS: readonly EmojiRow[] = [")
    for entry in entries:
        char = ts_string(str(entry["char"]))
        name = ts_string(str(entry["name"]))
        category = ts_string(str(entry["category"]))
        tonable = "1" if entry["tonable"] else "0"
        codes = ts_string(" ".join(entry["shortcodes"]))
        lines.append(f"  [{char}, {name}, {category}, {tonable}, {codes}],")
    lines.append("];")
    lines.append("")
    lines.append("export const STANDARD_EMOJI: readonly StandardEmoji[] = EMOJI_ROWS.map(")
    lines.append("  ([char, name, category, tonable, shortcodes]): StandardEmoji => ({")
    lines.append("    char,")
    lines.append("    name,")
    lines.append("    category,")
    lines.append("    tonable: tonable === 1,")
    lines.append('    shortcodes: shortcodes === "" ? [] : shortcodes.split(" "),')
    lines.append("  }),")
    lines.append(");")
    lines.append("")
    lines.append("/**")
    lines.append(" * Apply a skin tone index (0 = none) to an emoji, returning the toned character.")
    lines.append(" *")
    lines.append(" * The modifier is inserted directly after the first codepoint, which is correct")
    lines.append(" * for every base in STANDARD_EMOJI because they are all single-codepoint. It is")
    lines.append(" * NOT generally correct for ZWJ sequences (families, professions), where each")
    lines.append(" * person component takes its own modifier — this dataset contains none of those,")
    lines.append(" * so if ZWJ sequences are ever added this function must be revisited.")
    lines.append(" */")
    lines.append("export function applySkinTone(emoji: StandardEmoji, toneIndex: number): string {")
    lines.append("  if (!emoji.tonable || toneIndex <= 0) {")
    lines.append("    return emoji.char;")
    lines.append("  }")
    lines.append("  const tone = SKIN_TONES[toneIndex];")
    lines.append('  if (tone === undefined || tone === "") {')
    lines.append("    return emoji.char;")
    lines.append("  }")
    lines.append("  // Split on codepoints, not UTF-16 units, so the surrogate pair stays intact.")
    lines.append("  const codepoints = Array.from(emoji.char);")
    lines.append("  const first = codepoints[0];")
    lines.append("  if (first === undefined) {")
    lines.append("    return emoji.char;")
    lines.append("  }")
    lines.append('  return first + tone + codepoints.slice(1).join("");')
    lines.append("}")
    lines.append("")
    return "\n".join(lines)


def main() -> None:
    entries, counts, source, shortcode_version = build_entries()
    rendered = render(entries, counts, source, shortcode_version)
    for out_path in (OUTPUT_PATH, MOBILE_OUTPUT_PATH):
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(rendered, encoding="utf-8")

    tonable_total = sum(1 for entry in entries if entry["tonable"])
    print(f"unicodedata {unicodedata.unidata_version}")
    print(f"country names: {source}")
    for category_id, label in CATEGORY_LABELS:
        print(f"  {category_id:<9} {label:<11} {counts[category_id]:>5}")
    print(f"  {'TOTAL':<21} {len(entries):>5}")
    print(f"  {'skin-tone bases':<21} {tonable_total:>5}")
    alias_total = sum(len(entry["shortcodes"]) for entry in entries)
    print(f"  {'shortcode aliases':<21} {alias_total:>5}")
    print(f"wrote {OUTPUT_PATH}")


if __name__ == "__main__":
    main()
