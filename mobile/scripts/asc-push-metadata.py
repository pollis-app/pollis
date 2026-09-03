#!/usr/bin/env python3
"""Push App Store listing metadata through the App Store Connect API.

Companion to `asc-upload-screenshots.py`. Same auth, same platform trap, same
no-dependency approach — see that file's docstring for the JWT/OpenSSL details.

    mobile/scripts/asc-push-metadata.py --dry-run
    mobile/scripts/asc-push-metadata.py

The copy is NOT duplicated here. `docs/store-listing.md` is the source of truth
and this parses it, so the reviewed prose and the thing Apple receives cannot
drift. Two files claiming to be the listing is exactly how a store page ends up
disagreeing with the document that was signed off.

What it writes, to `appStoreVersionLocalizations` for one locale:

    description, keywords, promotionalText, supportUrl, marketingUrl

Deliberately NOT written:
  * `whatsNew` — meaningless on a first version, and wrong to invent.
  * The app-level `subtitle`/`privacyPolicyUrl` live on
    `appInfoLocalizations`, a different resource with a different lifecycle;
    they are pushed too, but through that endpoint rather than pretended to be
    version fields.
  * Age rating, App Privacy answers and export compliance are declarations, not
    copy. They are the owner's to make in the UI, and a script that quietly
    answered them would be forging a legal attestation.
"""

import argparse
import base64
import importlib.util
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DOC = os.path.normpath(os.path.join(HERE, "..", "..", "docs", "store-listing.md"))

# Reuse the sibling's auth/HTTP rather than copying it — one implementation of
# the JWT and one of the platform filter.
_spec = importlib.util.spec_from_file_location(
    "asc_upload", os.path.join(HERE, "asc-upload-screenshots.py")
)
asc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(asc)

# ASC rejects over-long fields with a generic 409, so check locally where the
# error can name the field and the overshoot.
LIMITS = {
    "subtitle": 30,
    "description": 4000,
    "keywords": 100,
    "promotionalText": 170,
}


def blockquote_after(md, heading_re, collapse=False):
    """Pull the `> ...` block that follows a heading, unwrapped.

    The doc writes every value as a blockquote under its heading, which is what
    makes it both readable and machine-extractable. Prose paragraphs after the
    quote are commentary for humans and are not part of the value.
    """
    m = re.search(heading_re, md)
    if not m:
        return None
    lines, started = [], False
    for line in md[m.end():].splitlines():
        if line.startswith(">"):
            started = True
            lines.append(line[1:].lstrip() if line[1:2] == " " else line[1:])
        elif started:
            break
    if not lines:
        return None
    # Inline code fences are formatting, not content. Strip them even when the
    # value is soft-wrapped across several quote lines — the doc wraps at ~95
    # columns for readability, which says nothing about the value's shape.
    text = "\n".join(lines).strip()
    if text.startswith("`") and text.endswith("`"):
        text = text[1:-1].strip()
    if collapse:
        # A single-line field: the newlines are the doc's wrapping, not content.
        # Collapsing matters beyond looks — a literal newline in `keywords` or
        # `subtitle` is a validation error at Apple, and one in promotional text
        # would ship as a line break nobody wrote.
        text = re.sub(r"\s*\n\s*", " ", text)
    return text.strip()


def parse_listing():
    if not os.path.isfile(DOC):
        asc.die(f"{DOC} not found")
    md = open(DOC).read()
    fields = {
        "subtitle": blockquote_after(md, r"### App Store subtitle[^\n]*\n", collapse=True),
        "description": blockquote_after(md, r"### Full description[^\n]*\n"),
        "keywords": blockquote_after(md, r"### App Store keywords[^\n]*\n", collapse=True),
        "promotionalText": blockquote_after(md, r"### App Store promotional text[^\n]*\n", collapse=True),
    }
    missing = [k for k, v in fields.items() if not v]
    if missing:
        asc.die(f"could not parse from {os.path.basename(DOC)}: {', '.join(missing)}")

    over = [f"{k} is {len(v)} chars, limit {LIMITS[k]}"
            for k, v in fields.items() if len(v) > LIMITS[k]]
    if over:
        asc.die("copy exceeds App Store limits:\n  " + "\n  ".join(over))
    return fields


def urls_from_doc():
    """Support / marketing / privacy URLs out of the doc's URLs section."""
    md = open(DOC).read()
    section = md[md.index("### URLs"):] if "### URLs" in md else ""
    found = {}
    for label, key in (("support", "supportUrl"), ("marketing", "marketingUrl"),
                       ("privacy", "privacyPolicyUrl")):
        m = re.search(rf"(?i)\|\s*{label}[^|]*\|\s*`?(https://[^\s`|]+)`?", section)
        if m:
            found[key] = m.group(1)
    return found


def main():
    ap = argparse.ArgumentParser(description="Push App Store listing metadata via the ASC API.")
    ap.add_argument("--bundle-id", default="com.pollis.mobile")
    ap.add_argument("--locale", default="en-US")
    ap.add_argument("--platform", default="IOS")
    ap.add_argument("--version", help="version string; default is the editable version")
    ap.add_argument("--dry-run", action="store_true", help="show what would be sent")
    args = ap.parse_args()

    fields = parse_listing()
    urls = urls_from_doc()

    print(f"Parsed from docs/store-listing.md:")
    for k, v in fields.items():
        preview = v.replace("\n", " ")[:70]
        print(f"  {k:<16} {len(v):>4}/{LIMITS[k]:<5} {preview}{'…' if len(v) > 70 else ''}")
    for k, v in urls.items():
        print(f"  {k:<16} {'':>10} {v}")

    token = asc.make_token(asc.secret("ASC_KEY_ID"), asc.secret("ASC_ISSUER_ID"),
                           base64.b64decode(asc.secret("ASC_KEY_P8_BASE64")).decode())

    apps = asc.api(token, "GET", f"/apps?filter[bundleId]={args.bundle_id}")["data"]
    if not apps:
        asc.die(f"no app with bundleId {args.bundle_id}")
    app_id = apps[0]["id"]

    versions = asc.api(token, "GET", f"/apps/{app_id}/appStoreVersions?limit=20")["data"]
    editable = {"PREPARE_FOR_SUBMISSION", "DEVELOPER_REJECTED", "REJECTED",
                "METADATA_REJECTED", "WAITING_FOR_REVIEW", "INVALID_BINARY"}
    # Platform filter: an app can hold one version record per platform, and
    # they are otherwise indistinguishable. See asc-upload-screenshots.py.
    cands = [v for v in versions
             if v["attributes"].get("platform") == args.platform
             and (not args.version or v["attributes"]["versionString"] == args.version)
             and v["attributes"]["appStoreState"] in editable]
    if not cands:
        asc.die(f"no editable {args.platform} version found")
    version_id = cands[0]["id"]
    print(f"\nversion: {cands[0]['attributes']['versionString']} "
          f"({args.platform}, {cands[0]['attributes']['appStoreState']})")

    locs = asc.api(token, "GET",
                   f"/appStoreVersions/{version_id}/appStoreVersionLocalizations"
                   f"?filter[locale]={args.locale}")["data"]
    if not locs:
        asc.die(f"no {args.locale} localization on that version")
    loc_id = locs[0]["id"]

    version_attrs = {
        "description": fields["description"],
        "keywords": fields["keywords"],
        "promotionalText": fields["promotionalText"],
    }
    for key in ("supportUrl", "marketingUrl"):
        if key in urls:
            version_attrs[key] = urls[key]

    # Subtitle and privacy policy are APP-level, not version-level.
    info = asc.api(token, "GET", f"/apps/{app_id}/appInfos")["data"]
    info_loc_id = None
    if info:
        il = asc.api(token, "GET",
                     f"/appInfos/{info[0]['id']}/appInfoLocalizations"
                     f"?filter[locale]={args.locale}")["data"]
        if il:
            info_loc_id = il[0]["id"]

    if args.dry_run:
        print("\nwould PATCH appStoreVersionLocalizations:", ", ".join(version_attrs))
        print("would PATCH appInfoLocalizations:",
              "subtitle" + (", privacyPolicyUrl" if "privacyPolicyUrl" in urls else "")
              if info_loc_id else "(no app info localization found)")
        print("dry run — nothing written")
        return

    asc.api(token, "PATCH", f"/appStoreVersionLocalizations/{loc_id}", {
        "data": {"type": "appStoreVersionLocalizations", "id": loc_id,
                 "attributes": version_attrs}
    })
    print(f"\nwrote {', '.join(version_attrs)} to the {args.locale} version localization")

    if info_loc_id:
        info_attrs = {"subtitle": fields["subtitle"]}
        if "privacyPolicyUrl" in urls:
            info_attrs["privacyPolicyUrl"] = urls["privacyPolicyUrl"]
        asc.api(token, "PATCH", f"/appInfoLocalizations/{info_loc_id}", {
            "data": {"type": "appInfoLocalizations", "id": info_loc_id,
                     "attributes": info_attrs}
        })
        print(f"wrote {', '.join(info_attrs)} to the {args.locale} app info localization")
    else:
        print("no app info localization found — subtitle not written")

    print("\nStill the owner's to do in ASC (declarations, not copy): age rating, "
          "App Privacy answers, export compliance, pricing and availability.")


if __name__ == "__main__":
    main()
