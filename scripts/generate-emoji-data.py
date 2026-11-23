#!/usr/bin/env python3
"""Generate ``frontend/src/components/Emoji/emojiData.ts``.

What this does
--------------
Reads Unicode's official emoji list, ``emoji-test.txt``, vendored verbatim as
``scripts/emoji-test.txt``, and writes every fully-qualified emoji in it (the
RGI set, Emoji 16.0) as a typed, dependency-free TypeScript module for the
Discord-style emoji picker. Run it from anywhere with Python 3.14 or newer:

    python3.14 scripts/generate-emoji-data.py

The generator is deterministic and offline: given the same vendored files and
the same ``unicodedata`` build it re-emits byte-identical output, so re-running
it on an unchanged tree produces an empty diff.

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

Where the emoji set comes from
------------------------------
``scripts/emoji-test.txt`` is Unicode's ``emoji-test.txt`` for Emoji 16.0, byte
for byte. ``scripts/emoji-test.json`` records where it came from (URL, version,
sha256) and the licence it is used under (Unicode-3.0), and the generator
refuses to run when the file's sha256 no longer matches. Refresh both with::

    python3.14 scripts/generate-emoji-data.py --refresh-emoji-test

after moving ``EMOJI_TEST_VERSION``. That mode is network-only; a plain run
stays offline.

The table is every ``fully-qualified`` line of the file, in the file's own
order (the CLDR order Slack, Discord and the system keyboards use), minus the
lines that carry a skin-tone modifier, which are reached through ``tonable``
(below). Of the ``component`` lines, the bare tones are skipped and the four
hair styles kept (filed under People): they are not RGI emoji, but every emoji
font draws them and the table has always carried them. So every ZWJ sequence (🧑‍💻, ❤️‍🔥, 😮‍💨, 🐦‍🔥, the
families), every keycap and the tag-sequence flags are in the table, and the
count is checked against the file (``frontend/tests/emoji-coverage.test.ts``).

Categories are the file's CLDR groups, with "Smileys & Emotion" and
"People & Body" folded into one ``people`` group as Discord and Slack do.
Within a category, entries keep the file's order.

Names: a single-codepoint emoji keeps its ``unicodedata.name()``, lowercased
with hyphens and underscores folded to spaces ("WHITE UP POINTING BACKHAND
INDEX" -> "white up pointing backhand index"), because the raw Unicode name
matches more useful queries than the CLDR short name ("backhand index pointing
up"). A sequence has no Unicode name, so it takes the file's CLDR name, folded
the same way ("woman: red hair"), and a flag is "<place> flag": the tzdata
short name for a country flag, as the table has always used ("korea (south)
flag"), else the CLDR one ("united nations flag"). This name is the English search fallback; the localized names and
keywords come from the CLDR annotations (below).

``char`` and U+FE0F
-------------------
A single-codepoint emoji's ``char`` is the bare codepoint with **no** U+FE0F,
as it has always been, so stored reactions, recents and the shortcode keys keep
matching. The renderers add the selector at display time: ``emojiDisplayChar``
appends it to single codepoints below U+1F000, which default to text
presentation. A sequence's ``char`` is its fully-qualified form exactly as the
file spells it ("❤️‍🔥" = 2764 FE0F 200D 1F525, "#️⃣" = 0023 FE0F 20E3): a
selector inside a sequence is part of what makes it render as one glyph, and
``emojiDisplayChar`` passes multi-codepoint strings through untouched. Every
key into the vendored JSON tables (shortcodes, annotations) is ``char`` with
U+FE0F stripped.

Skin tones
----------
An entry is ``tonable`` when the file lists it in all five *uniform* tones
(every person in it the same tone), the only kind the picker's one skin-tone
choice can ask for; mixed-tone forms such as 🧑🏻‍🤝‍🧑🏿 are not offered.
``applySkinTone`` puts the tone straight after the first codepoint and drops a
U+FE0F that followed it (☝ -> ☝🏻, 🏃‍♀️ -> 🏃🏻‍♀️, ⛹️‍♂️ -> ⛹🏻‍♂️), which is right for every
one-person emoji. Where each person takes the tone (🧑‍🤝‍🧑, 👩‍❤️‍👨 and the like)
the generator emits the light-tone form in ``TONE_TEMPLATES`` and the runtime
swaps the chosen tone in. The generator checks both rules against the file for
all five tones, so every toned string the picker can produce is one the file
lists as fully-qualified.

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
``"joy": "1F602"`` and ``"gb": "1F1EC-1F1E7"``, with U+FE0F stripped, the key form
every vendored table uses (see "``char`` and U+FE0F"). Aliases are lowercase
``[a-z0-9_+-]``; note that ``+`` and ``-`` are in that set (``:+1:``, ``:-1:``,
``:e-mail:``) and are NOT legal in a *custom* emoji shortcode, which stays
``[a-z0-9_]{2,32}``. Single-character aliases (``:v:``, ``:o:``, ``:x:``) are
kept: they are real, and the two-character minimum before the composer starts
suggesting is a UI rule, not a data rule.

An alias whose target emoji is not in the emitted table (a skin-toned
variant, which the picker composes from its base) is simply dropped, so the
emitted set is always a subset of what is renderable.

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

Where the localized names and keywords come from
------------------------------------------------
``unicodedata.name()`` is English, so on its own the picker's search only works
for English queries even after the picker chrome is localized (#901). The
per-locale display name and search keywords come from **CLDR emoji
annotations** — ``common/annotations/<lang>.xml`` (the ``tts`` entry is the
name, the plain entry the ``|``-separated keywords) plus
``common/annotationsDerived/<lang>.xml``, which is where CLDR files the flags.

They are vendored, trimmed, in ``scripts/emoji-annotations.json`` for the same
reasons the shortcodes are: the full CLDR files run to several MB per locale
and cover thousands of sequences this table does not emit, while the trimmed
JSON is static, diffable, and needs no install step. The JSON carries its own
provenance (release tag, per-file sha256, licence) and is keyed
``locale -> hyphen-joined uppercase codepoints -> [name, keyword, ...]``,
lowercased, with U+FE0F stripped as CLDR's own ``cp`` attribute is; the emitted
modules carry each entry's ``char``. Entries CLDR marks ``draft="provisional"`` / ``"unconfirmed"`` are
skipped; ``contributed`` is kept.

The locale set is read from ``frontend/src/i18n/languages.ts``, so adding a
locale there and refreshing is the whole procedure::

    python3 scripts/generate-emoji-data.py --refresh-annotations

That mode is the ONLY one that touches the network (stdlib ``urllib``, pinned
to ``CLDR_RELEASE``); a plain run stays offline and deterministic. Both modes
then emit one ``annotations/<lang>.ts`` per locale plus an ``index.ts`` whose
loader dynamically imports them, under both ``frontend/src/components/Emoji/``
and ``mobile/components/emoji/``. Each table is ~90-190 KB and the picker only
ever needs the active locale's and English's, which on the web bundle is a
separate chunk per locale (#874's code-splitting rule); Metro does not split,
so on mobile every locale ships in the binary and the dynamic import only
defers parsing until the picker opens.

Known limits
------------
*   The newest emoji and the tag-sequence flags (England, Scotland, Wales) need
    a font that has them. Apple platforms and current Chrome and Android builds
    do; an older Android system font draws a missing one as a box or as its
    pieces. See ``.codesight/wiki/ui.md`` -> "Emoji coverage".
*   The ``:shortcode:`` aliases are gemoji's, which stop before Emoji 15.1, so
    newer emoji (🫩, 🐦‍🔥, 🙂‍↔️ ...) are found by name, not by shortcode.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import unicodedata
import urllib.request
import xml.etree.ElementTree as ET
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

# Unicode's emoji-test.txt, vendored verbatim, plus its provenance. See "Where
# the emoji set comes from" above.
EMOJI_TEST_PATH = REPO_ROOT / "scripts" / "emoji-test.txt"
EMOJI_TEST_META_PATH = REPO_ROOT / "scripts" / "emoji-test.json"
EMOJI_TEST_VERSION = "16.0"
EMOJI_TEST_URL = f"https://unicode.org/Public/emoji/{EMOJI_TEST_VERSION}/emoji-test.txt"

# The vendored, trimmed CLDR annotation table. See "Where the localized names
# and keywords come from" above.
ANNOTATIONS_PATH = REPO_ROOT / "scripts" / "emoji-annotations.json"

# One generated module per locale plus the loader index, per app. Both
# directories are owned by this script: stale locale modules are removed on
# every run. Mobile gets the same modules because its picker is localized
# through the same shared catalogues (#1074).
ANNOTATIONS_DIRS = (
    REPO_ROOT / "frontend" / "src" / "components" / "Emoji" / "annotations",
    REPO_ROOT / "mobile" / "components" / "emoji" / "annotations",
)

# The locale registry. `--refresh-annotations` reads its `code: "xx"` entries
# so the annotation set can never drift from the languages the app ships.
LANGUAGES_TS = REPO_ROOT / "frontend" / "src" / "i18n" / "languages.ts"

CLDR_RELEASE = "release-48"
# The Unicode version the committed tables were built from; an older Python refuses to run (main).
MIN_UNICODE_VERSION = (16, 0, 0)
CLDR_RAW_BASE = f"https://raw.githubusercontent.com/unicode-org/cldr/{CLDR_RELEASE}/common"
CLDR_ANNOTATION_FILES = ("annotations", "annotationsDerived")
CLDR_SKIPPED_DRAFTS = {"provisional", "unconfirmed"}

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

# The file's CLDR groups, mapped onto the picker's categories. "Component"
# (the bare tones and hair styles) is deliberately absent.
GROUP_CATEGORIES: dict[str, str] = {
    "Smileys & Emotion": "people",
    "People & Body": "people",
    "Animals & Nature": "nature",
    "Food & Drink": "food",
    "Travel & Places": "travel",
    "Activities": "activity",
    "Objects": "objects",
    "Symbols": "symbols",
    "Flags": "flags",
}

# The five Fitzpatrick modifiers, in Discord's order. Index 0 is "no tone".
SKIN_TONES = ["", "\U0001F3FB", "\U0001F3FC", "\U0001F3FD", "\U0001F3FE", "\U0001F3FF"]
TONE_SET = frozenset(SKIN_TONES[1:])
LIGHT_TONE = SKIN_TONES[1]
VS16 = "️"


# English names for the regional-indicator flags, in the tzdata short form the
# table has always used ("korea (south)", "britain (uk)"), so a search that
# found a flag before still does. A flag missing here (AC, EU, UN, ...) takes
# its CLDR name.
COUNTRY_NAMES: dict[str, str] = {
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

REGIONAL_INDICATOR_A = 0x1F1E6


def strip_vs16(value: str) -> str:
    """``value`` without any U+FE0F, the form every vendored JSON key uses."""
    return value.replace(VS16, "")


def display_name(codepoint: int) -> str | None:
    """Unicode name, lowercased and normalised, or None if unassigned."""
    try:
        raw = unicodedata.name(chr(codepoint))
    except ValueError:
        return None
    return fold_name(raw)


def fold_name(raw: str) -> str:
    return " ".join(raw.lower().replace("-", " ").replace("_", " ").split())


def load_shortcodes() -> tuple[dict[str, list[str]], str]:
    """Invert the vendored alias table into ``FE0F-stripped char -> sorted aliases``.

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


def read_emoji_test() -> str:
    """The vendored emoji-test.txt, after checking it against its recorded sha256."""
    raw = EMOJI_TEST_PATH.read_bytes()
    meta = json.loads(EMOJI_TEST_META_PATH.read_text(encoding="utf-8"))
    digest = hashlib.sha256(raw).hexdigest()
    if digest != meta["_sha256"]:
        raise SystemExit(
            f"{EMOJI_TEST_PATH} has sha256 {digest}, but {EMOJI_TEST_META_PATH} records "
            f"{meta['_sha256']}; refresh both with --refresh-emoji-test"
        )
    return raw.decode("utf-8")


def refresh_emoji_test() -> None:
    """Re-download ``emoji-test.txt`` and rewrite its provenance (network)."""
    with urllib.request.urlopen(EMOJI_TEST_URL, timeout=60) as response:
        raw = response.read()
    EMOJI_TEST_PATH.write_bytes(raw)
    doc = {
        "_source": EMOJI_TEST_URL,
        "_version": EMOJI_TEST_VERSION,
        "_sha256": hashlib.sha256(raw).hexdigest(),
        "_licence": "Unicode-3.0 — Copyright © 1991-2024 Unicode, Inc. (https://www.unicode.org/license.txt)",
        "_file": "scripts/emoji-test.txt, vendored byte for byte",
        "_regenerate": "python3.14 scripts/generate-emoji-data.py --refresh-emoji-test",
    }
    EMOJI_TEST_META_PATH.write_text(
        json.dumps(doc, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )


def parse_emoji_test(text: str) -> list[tuple[str, str, str, str]]:
    """``(emoji, status, group, cldr name)`` for every data line, in file order."""
    rows: list[tuple[str, str, str, str]] = []
    group = ""
    for line in text.splitlines():
        if line.startswith("# group:"):
            group = line.split(":", 1)[1].strip()
            continue
        if not line.strip() or line.startswith("#"):
            continue
        codepoints, rest = line.split(";", 1)
        status, comment = rest.split("#", 1)
        emoji = "".join(chr(int(part, 16)) for part in codepoints.split())
        # The comment is "<emoji> E<version> <name>".
        name = comment.strip().split(" ", 2)[2]
        rows.append((emoji, status.strip(), group, name))
    return rows


def simple_tone(char: str, tone: str) -> str:
    """The runtime's default rule: the tone after the first codepoint, minus a U+FE0F there."""
    codepoints = list(char)
    rest = codepoints[1:]
    if rest and rest[0] == VS16:
        rest = rest[1:]
    return codepoints[0] + tone + "".join(rest)


def uniform_tone_variants(rows: list[tuple[str, str, str, str]]) -> dict[str, dict[str, str]]:
    """``FE0F-stripped base -> tone -> toned form``, for forms whose people all share one tone."""
    variants: dict[str, dict[str, str]] = {}
    for emoji, status, _group, _name in rows:
        if status != "fully-qualified":
            continue
        tones = {c for c in emoji if c in TONE_SET}
        if len(tones) != 1:
            continue
        (tone,) = tones
        base = strip_vs16("".join(c for c in emoji if c not in TONE_SET))
        slot = variants.setdefault(base, {})
        if tone in slot:
            raise SystemExit(f"two {tone!r} forms for one base: {slot[tone]!r} and {emoji!r}")
        slot[tone] = emoji
    return variants


def tone_rule(char: str, variants: dict[str, dict[str, str]]) -> tuple[bool, str | None]:
    """``(tonable, template)``: template is None when the default rule is right for every tone."""
    forms = variants.get(strip_vs16(char), {})
    if len(forms) != len(TONE_SET):
        return False, None
    if all(simple_tone(char, tone) == forms[tone] for tone in TONE_SET):
        return True, None
    template = forms[LIGHT_TONE]
    for tone in TONE_SET:
        if template.replace(LIGHT_TONE, tone) != forms[tone]:
            raise SystemExit(f"no single tone rule for {char!r}")
    return True, template


def entry_char(emoji: str) -> str:
    """A single codepoint stays bare (see "char and U+FE0F"); a sequence keeps its selectors."""
    bare = strip_vs16(emoji)
    return bare if len(bare) == 1 else emoji


def entry_name(char: str, cldr_name: str) -> str:
    if len(char) == 1:
        name = display_name(ord(char))
        if name is not None:
            return name
    if len(char) == 2 and all(0 <= ord(c) - REGIONAL_INDICATOR_A < 26 for c in char):
        code = "".join(chr(ord("A") + ord(c) - REGIONAL_INDICATOR_A) for c in char)
        if code in COUNTRY_NAMES:
            return f"{COUNTRY_NAMES[code].lower()} flag"
    if cldr_name.startswith("flag: "):
        return fold_name(f"{cldr_name[len('flag: '):]} flag")
    return fold_name(cldr_name)


def build_entries() -> tuple[list[dict[str, object]], dict[str, int], str, dict[str, int]]:
    """Every fully-qualified, tone-free emoji in emoji-test.txt, by category then file order.

    Also returns the shortcode table version and the line counts the header
    reconciles the total against.
    """
    rows = parse_emoji_test(read_emoji_test())
    variants = uniform_tone_variants(rows)
    shortcodes, shortcode_version = load_shortcodes()
    by_category: dict[str, list[dict[str, object]]] = {cid: [] for cid, _ in CATEGORY_LABELS}
    seen: set[str] = set()
    fully_qualified = 0
    components = 0

    for emoji, status, group, cldr_name in rows:
        if status == "fully-qualified":
            fully_qualified += 1
        elif status == "component" and emoji not in TONE_SET:
            components += 1
            # The four hair styles (🦰 🦱 🦳 🦲) are components, not RGI emoji,
            # but every emoji font draws them on their own and the table has
            # carried them since it was first generated, so they stay, filed
            # under People where CLDR's Component group would sit.
            group = "People & Body"
        else:
            continue
        if any(c in TONE_SET for c in emoji):
            continue
        category = GROUP_CATEGORIES.get(group)
        if category is None:
            raise SystemExit(f"emoji-test.txt group {group!r} has no picker category")
        char = entry_char(emoji)
        key = strip_vs16(char)
        if key in seen:
            raise SystemExit(f"{emoji!r} is listed twice")
        seen.add(key)
        tonable, template = tone_rule(char, variants)
        by_category[category].append(
            {
                "char": char,
                "name": entry_name(char, cldr_name),
                "category": category,
                "tonable": tonable,
                "tone_template": template,
                "shortcodes": shortcodes.get(key, []),
            }
        )

    entries = [entry for cid, _ in CATEGORY_LABELS for entry in by_category[cid]]
    counts = {cid: len(by_category[cid]) for cid, _ in CATEGORY_LABELS}
    toned = fully_qualified - (len(entries) - components)
    stats = {"fully_qualified": fully_qualified, "toned": toned, "components": components}
    return entries, counts, shortcode_version, stats


def codepoint_key(char: str) -> str:
    """The JSON key for a character: hyphen-joined uppercase hex, U+FE0F stripped, as gemoji."""
    return "-".join(f"{ord(c):04X}" for c in strip_vs16(char))


def shipped_language_codes() -> list[str]:
    """The `code: "xx"` / `"xx-Scrp"` entries of ``SHIPPED_LANGUAGES`` in languages.ts (#1725)."""
    source = LANGUAGES_TS.read_text(encoding="utf-8")
    codes = re.findall(r'^\s*\{\s*code:\s*"([a-z]{2,3}(?:-[A-Z][a-z]{3})?)"', source, flags=re.MULTILINE)
    if not codes:
        raise SystemExit(f"no `code:` entries found in {LANGUAGES_TS}")
    return sorted(set(codes))


# A shipped code whose CLDR file has another name: CLDR files a language's
# default script under the bare tag, so Gurmukhi Punjabi is `pa.xml` (#1726).
CLDR_FILE_OVERRIDES = {"pa-Guru": "pa"}

# Where CLDR has no name for an emoji in a locale: Shahmukhi Punjabi (about a
# third missing in release 48) takes the Urdu name, which its readers read in
# the same script; Marathi takes the Hindi one (Devanagari).
CLDR_GAP_FALLBACKS = {"pa-Arab": "ur", "mr": "hi"}


def fetch_cldr_file(lang: str, kind: str) -> bytes:
    # CLDR names script variants with an underscore: `pa-Arab` is `pa_Arab.xml`.
    name = CLDR_FILE_OVERRIDES.get(lang, lang).replace("-", "_")
    url = f"{CLDR_RAW_BASE}/{kind}/{name}.xml"
    with urllib.request.urlopen(url, timeout=60) as response:
        return response.read()


def parse_cldr_annotations(
    payload: bytes, wanted: dict[str, str]
) -> dict[str, tuple[str | None, list[str]]]:
    """``key -> (tts name, keywords)`` for the wanted chars only.

    ``wanted`` maps the bare character to its JSON key. CLDR already strips
    U+FE0F from ``cp``, so the lookup is a plain dict hit.
    """
    out: dict[str, tuple[str | None, list[str]]] = {}
    root = ET.fromstring(payload)
    for node in root.iter("annotation"):
        cp = node.get("cp", "")
        key = wanted.get(cp)
        if key is None:
            continue
        if node.get("draft") in CLDR_SKIPPED_DRAFTS:
            continue
        text = (node.text or "").strip().lower()
        if not text:
            continue
        name, keywords = out.get(key, (None, []))
        if node.get("type") == "tts":
            name = text
        else:
            for keyword in text.split("|"):
                keyword = keyword.strip()
                if keyword and keyword not in keywords:
                    keywords.append(keyword)
        out[key] = (name, keywords)
    return out


def refresh_annotations(entries: list[dict[str, object]]) -> None:
    """Download the CLDR files for every shipped locale and rewrite the JSON."""
    wanted = {strip_vs16(str(entry["char"])): codepoint_key(str(entry["char"])) for entry in entries}
    locales: dict[str, dict[str, list[str]]] = {}
    digests: dict[str, dict[str, str]] = {}
    for lang in shipped_language_codes():
        merged: dict[str, tuple[str | None, list[str]]] = {}
        digests[lang] = {}
        for kind in CLDR_ANNOTATION_FILES:
            payload = fetch_cldr_file(lang, kind)
            digests[lang][kind] = hashlib.sha256(payload).hexdigest()
            for key, (name, keywords) in parse_cldr_annotations(payload, wanted).items():
                prior_name, prior_keywords = merged.get(key, (None, []))
                merged[key] = (
                    name or prior_name,
                    prior_keywords + [k for k in keywords if k not in prior_keywords],
                )
        table: dict[str, list[str]] = {}
        for key in sorted(merged):
            name, keywords = merged[key]
            if name is None:
                continue
            # The name is searched on its own, so a keyword repeating it is
            # dead weight in every emitted module.
            table[key] = [name] + [k for k in keywords if k != name]
        locales[lang] = table
        print(f"annotations {lang}: {len(table)}/{len(entries)} entries", file=sys.stderr)

    # CLDR leaves some locales short; fill each gap from a shipped language in
    # the same script that the readers also read, never from English (#1726).
    for lang, fallback in CLDR_GAP_FALLBACKS.items():
        if lang not in locales or fallback not in locales:
            continue
        gaps = {key: values for key, values in locales[fallback].items() if key not in locales[lang]}
        if gaps:
            locales[lang] = dict(sorted({**locales[lang], **gaps}.items()))
            print(f"annotations {lang}: {len(gaps)} gaps filled from {fallback}", file=sys.stderr)

    doc = {
        "_source": "https://github.com/unicode-org/cldr — common/annotations/<lang>.xml + common/annotationsDerived/<lang>.xml (flags)",
        "_release": CLDR_RELEASE,
        "_licence": "Unicode-3.0 — Copyright © 1991-2025 Unicode, Inc.",
        "_shape": "locale -> hyphen-joined uppercase codepoints of the base emoji (U+FE0F stripped) -> [name, keyword, ...], lowercased; only the codepoints emojiData.ts carries",
        "_regenerate": "python3 scripts/generate-emoji-data.py --refresh-annotations (locales come from frontend/src/i18n/languages.ts)",
        "_sha256": digests,
        "locales": locales,
    }
    ANNOTATIONS_PATH.write_text(
        json.dumps(doc, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )


def load_annotations() -> tuple[dict[str, dict[str, list[str]]], str]:
    payload = json.loads(ANNOTATIONS_PATH.read_text(encoding="utf-8"))
    return payload["locales"], str(payload["_release"])


def annotation_header(release: str) -> list[str]:
    return [
        "// GENERATED FILE — DO NOT EDIT BY HAND.",
        "//",
        "// Produced by `scripts/generate-emoji-data.py` from the vendored CLDR emoji",
        f"// annotations in scripts/emoji-annotations.json (CLDR {release}). To refresh",
        "// the data, or after adding a locale to i18n/languages.ts:",
        "//",
        "//     python3 scripts/generate-emoji-data.py --refresh-annotations",
        "//",
    ]


def render_annotation_module(
    lang: str, table: dict[str, list[str]], chars: dict[str, str], release: str
) -> str:
    """One locale's `[char, name, keywords]` rows; ``chars`` maps a key to its entry's ``char``."""
    lines = annotation_header(release)
    lines.append(f"// Locale: {lang} — {len(table)} entries")
    lines.append("")
    lines.append('import type { EmojiAnnotationRow } from "./index";')
    lines.append("")
    lines.append("const ROWS: readonly EmojiAnnotationRow[] = [")
    for key, values in table.items():
        char = chars[key]
        name, keywords = values[0], values[1:]
        lines.append(
            f"  [{ts_string(char)}, {ts_string(name)}, {ts_string('|'.join(keywords))}],"
        )
    lines.append("];")
    lines.append("")
    lines.append("export default ROWS;")
    lines.append("")
    return "\n".join(lines)


def render_annotation_index(langs: list[str], release: str) -> str:
    lines = annotation_header(release)
    lines.append("// Each locale is its own module behind a dynamic import. In the web bundle that")
    lines.append("// is a separate chunk (the picker needs the active locale's table and English's,")
    lines.append("// never all of them); on native, where Metro does not split, every locale ships")
    lines.append("// in the binary and the import only defers parsing until the picker opens.")
    lines.append("")
    lines.append("/** `[char, localized name, `|`-joined lowercase keywords]`. */")
    lines.append("export type EmojiAnnotationRow = readonly [")
    lines.append("  char: string,")
    lines.append("  name: string,")
    lines.append("  keywords: string,")
    lines.append("];")
    lines.append("")
    lines.append("export const EMOJI_ANNOTATION_LOCALES: readonly string[] = [")
    for lang in langs:
        lines.append(f"  {ts_string(lang)},")
    lines.append("];")
    lines.append("")
    lines.append("/** The rows for `locale`, or null when no table ships for it. */")
    lines.append("export function loadEmojiAnnotationRows(")
    lines.append("  locale: string,")
    lines.append("): Promise<readonly EmojiAnnotationRow[]> | null {")
    lines.append("  switch (locale) {")
    for lang in langs:
        lines.append(f"    case {ts_string(lang)}:")
        lines.append(f'      return import("./{lang}").then((m) => m.default);')
    lines.append("    default:")
    lines.append("      return null;")
    lines.append("  }")
    lines.append("}")
    lines.append("")
    return "\n".join(lines)


def write_annotations(entries: list[dict[str, object]]) -> dict[str, int]:
    """Emit every locale module and the index; drop modules for gone locales."""
    locales, release = load_annotations()
    # Keyed without U+FE0F, emitted with the entry's own `char` so a lookup by
    # `emoji.char` hits for sequences too.
    known = {codepoint_key(str(entry["char"])): str(entry["char"]) for entry in entries}
    langs = sorted(locales)
    counts: dict[str, int] = {}
    modules: dict[str, str] = {"index.ts": render_annotation_index(langs, release)}
    for lang in langs:
        # A key the table no longer carries (a narrowed range) is dropped here
        # rather than shipped as an orphan row.
        table = {key: values for key, values in locales[lang].items() if key in known}
        counts[lang] = len(table)
        modules[f"{lang}.ts"] = render_annotation_module(lang, table, known, release)
    for out_dir in ANNOTATIONS_DIRS:
        out_dir.mkdir(parents=True, exist_ok=True)
        for name, rendered in modules.items():
            (out_dir / name).write_text(rendered, encoding="utf-8")
        for stale in out_dir.glob("*.ts"):
            if stale.name not in modules:
                stale.unlink()
    return counts


def ts_string(value: str) -> str:
    """Quote a value as a double-quoted TypeScript string literal."""
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def render(
    entries: list[dict[str, object]],
    counts: dict[str, int],
    shortcode_version: str,
    stats: dict[str, int],
) -> str:
    """Render the whole TypeScript module."""
    total = len(entries)
    summary = ", ".join(f"{cid} {counts[cid]}" for cid, _label in CATEGORY_LABELS)
    with_shortcodes = sum(1 for entry in entries if entry["shortcodes"])
    alias_total = sum(len(entry["shortcodes"]) for entry in entries)
    tonable_total = sum(1 for entry in entries if entry["tonable"])
    templates = [entry for entry in entries if entry["tone_template"] is not None]
    meta = json.loads(EMOJI_TEST_META_PATH.read_text(encoding="utf-8"))

    lines: list[str] = []
    lines.append("// GENERATED FILE — DO NOT EDIT BY HAND.")
    lines.append("//")
    lines.append("// Produced by `scripts/generate-emoji-data.py`. To regenerate:")
    lines.append("//")
    lines.append("//     python3.14 scripts/generate-emoji-data.py")
    lines.append("//")
    lines.append(f"// Emoji set from:       scripts/emoji-test.txt (Emoji {meta['_version']}, sha256 {meta['_sha256'][:16]}…)")
    lines.append(f"// Unicode names from:   unicodedata {unicodedata.unidata_version}")
    lines.append(f"// Shortcodes from:      scripts/emoji-shortcodes.json ({shortcode_version})")
    lines.append(f"// Entries:              {total} ({summary})")
    lines.append(
        f"//                       = {stats['fully_qualified']} fully-qualified lines - {stats['toned']} skin-tone forms"
        f" + {stats['components']} hair styles"
    )
    lines.append(f"// Skin-tone bases:      {tonable_total} ({len(templates)} with a TONE_TEMPLATES form)")
    lines.append(f"// With shortcodes:      {with_shortcodes} entries, {alias_total} aliases")
    lines.append("//")
    lines.append("// Every fully-qualified emoji in Unicode's emoji-test.txt, in its CLDR groups")
    lines.append("// and order, skin-tone forms excepted (`tonable` + `applySkinTone` reach them).")
    lines.append("// Names are the English search fallback: the Unicode name for a single")
    lines.append("// codepoint, the CLDR name for a sequence. Localized names and keywords live in")
    lines.append("// the per-locale modules under `annotations/` (CLDR), loaded on demand.")
    lines.append("//")
    lines.append("// `char` for a single codepoint is the bare codepoint with no U+FE0F; entries")
    lines.append("// below U+1F000 default to text presentation, so a renderer wanting a colour")
    lines.append("// glyph appends U+FE0F at display time (`emojiDisplayChar`). `char` for a")
    lines.append("// sequence (ZWJ, keycap, flag) is its fully-qualified form, selectors included,")
    lines.append("// and is displayed as is.")
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
    lines.append("  /** The emoji itself: a bare codepoint, or a fully-qualified sequence. */")
    lines.append("  readonly char: string;")
    lines.append('  /** Human-readable name, lowercase (e.g. "grinning face"). */')
    lines.append("  readonly name: string;")
    lines.append("  readonly category: EmojiCategoryId;")
    lines.append("  /** True when the emoji comes in all five skin tones (see `applySkinTone`). */")
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
    lines.append(" * The tonable emoji where every person takes the tone (🧑‍🤝‍🧑, 👩‍❤️‍👨 …), as their")
    lines.append(" * light-tone (U+1F3FB) form; `applySkinTone` swaps the chosen tone in. Every")
    lines.append(" * other tonable emoji takes the tone after its first codepoint.")
    lines.append(" */")
    lines.append("const TONE_TEMPLATES: ReadonlyMap<string, string> = new Map([")
    for entry in templates:
        lines.append(f"  [{ts_string(str(entry['char']))}, {ts_string(str(entry['tone_template']))}],")
    lines.append("]);")
    lines.append("")
    lines.append("/**")
    lines.append(" * Apply a skin tone index (0 = none) to an emoji, returning the toned string.")
    lines.append(" *")
    lines.append(" * The generator checked both rules below against emoji-test.txt for all five")
    lines.append(" * tones, so the result is always a fully-qualified emoji Unicode lists.")
    lines.append(" */")
    lines.append("export function applySkinTone(emoji: StandardEmoji, toneIndex: number): string {")
    lines.append("  if (!emoji.tonable || toneIndex <= 0) {")
    lines.append("    return emoji.char;")
    lines.append("  }")
    lines.append("  const tone = SKIN_TONES[toneIndex];")
    lines.append('  if (tone === undefined || tone === "") {')
    lines.append("    return emoji.char;")
    lines.append("  }")
    lines.append("  const template = TONE_TEMPLATES.get(emoji.char);")
    lines.append("  if (template !== undefined) {")
    lines.append("    return template.split(SKIN_TONES[1]).join(tone);")
    lines.append("  }")
    lines.append("  // Split on codepoints, not UTF-16 units, so the surrogate pair stays intact.")
    lines.append("  const codepoints = Array.from(emoji.char);")
    lines.append("  const first = codepoints[0];")
    lines.append("  if (first === undefined) {")
    lines.append("    return emoji.char;")
    lines.append("  }")
    lines.append("  // A modifier implies emoji presentation, so a U+FE0F after the base goes.")
    lines.append('  const rest = codepoints[1] === "\\u{FE0F}" ? codepoints.slice(2) : codepoints.slice(1);')
    lines.append('  return first + tone + rest.join("");')
    lines.append("}")
    lines.append("")
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument(
        "--refresh-annotations",
        action="store_true",
        help="re-download the CLDR annotations for every shipped locale (network)",
    )
    parser.add_argument(
        "--refresh-emoji-test",
        action="store_true",
        help="re-download Unicode's emoji-test.txt and rewrite scripts/emoji-test.json (network)",
    )
    args = parser.parse_args()
    if args.refresh_emoji_test:
        refresh_emoji_test()

    # Single-codepoint names come from this Python's `unicodedata`: an older
    # Python (macOS's /usr/bin/python3 is 3.9, Unicode 13) would fall back to
    # the CLDR names for every newer emoji and churn the tables, so refuse (#1726).
    have = tuple(int(part) for part in unicodedata.unidata_version.split("."))
    if have < MIN_UNICODE_VERSION:
        want = ".".join(str(part) for part in MIN_UNICODE_VERSION)
        raise SystemExit(
            f"this Python has Unicode {unicodedata.unidata_version} data; the tables need {want}+ "
            "(run it with Python 3.14 or newer)"
        )

    entries, counts, shortcode_version, stats = build_entries()
    if args.refresh_annotations:
        refresh_annotations(entries)
    rendered = render(entries, counts, shortcode_version, stats)
    for out_path in (OUTPUT_PATH, MOBILE_OUTPUT_PATH):
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(rendered, encoding="utf-8")
    annotation_counts = write_annotations(entries)

    tonable_total = sum(1 for entry in entries if entry["tonable"])
    print(f"emoji-test.txt {EMOJI_TEST_VERSION}: {stats['fully_qualified']} fully-qualified lines")
    print(f"unicodedata {unicodedata.unidata_version}")
    for category_id, label in CATEGORY_LABELS:
        print(f"  {category_id:<9} {label:<11} {counts[category_id]:>5}")
    print(f"  {'TOTAL':<21} {len(entries):>5}")
    print(f"  {'skin-tone bases':<21} {tonable_total:>5}")
    alias_total = sum(len(entry["shortcodes"]) for entry in entries)
    print(f"  {'shortcode aliases':<21} {alias_total:>5}")
    for lang, count in annotation_counts.items():
        print(f"  {'annotations ' + lang:<21} {count:>5}")
    print(f"wrote {OUTPUT_PATH}")
    for out_dir in ANNOTATIONS_DIRS:
        print(f"wrote {out_dir}/")


if __name__ == "__main__":
    main()
