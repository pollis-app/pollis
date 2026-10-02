#!/usr/bin/env python3
"""Drive the Google Play Console from the CLI via the Play Developer API.

The Android counterpart of asc-upload-screenshots.py / asc-push-metadata.py:

    mobile/scripts/play-publish.py upload  [--track alpha] [--status draft] [AAB]
    mobile/scripts/play-publish.py listing [--icon PNG] [--feature-graphic PNG]
                                           [--phone PNG ...] [--dry-run]

Every change goes through one Play "edit" (a transaction): open, mutate,
commit. A failure before the commit discards the edit, so the console is never
left half-updated.

Credentials: the service-account JSON in Doppler `PLAY_SERVICE_ACCOUNT_JSON`
(pollis/prd_prod), or the same JSON in the environment. The account is
`play-publisher@pollis.iam.gserviceaccount.com`, invited in Play Console →
Users and permissions with app-level release + store-presence rights. A copy of
the key is in 1Password ("Pollis — Play service account"). Nothing is printed.

DEPENDENCIES: none. Stdlib plus the `openssl` binary, like the ASC scripts —
Google wants an RS256 JWT exchanged for an OAuth token, and `openssl dgst`
signs it without PyJWT/cryptography.

Copy comes from docs/store-listing.md through asc-push-metadata.py's parser,
so both stores ship the same words from one source.
"""

import argparse
import base64
import importlib.util
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
PACKAGE = "com.pollis.mobile"
API = f"https://androidpublisher.googleapis.com/androidpublisher/v3/applications/{PACKAGE}"
UPLOAD_API = f"https://androidpublisher.googleapis.com/upload/androidpublisher/v3/applications/{PACKAGE}"
DEFAULT_AAB = os.path.join(HERE, "..", "android", "app", "build", "outputs", "bundle", "release", "app-release.aab")

# Play's own field limits; checked locally so the error names the field.
LIMITS = {"title": 30, "shortDescription": 80, "fullDescription": 4000}


def die(msg):
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(1)


def b64u(raw):
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()


def service_account():
    raw = os.environ.get("PLAY_SERVICE_ACCOUNT_JSON")
    if not raw:
        try:
            raw = subprocess.run(
                ["doppler", "secrets", "get", "PLAY_SERVICE_ACCOUNT_JSON",
                 "-p", "pollis", "-c", "prd_prod", "--plain"],
                capture_output=True, text=True, check=True,
            ).stdout
        except (subprocess.CalledProcessError, FileNotFoundError):
            die("PLAY_SERVICE_ACCOUNT_JSON not in the environment and could not be read from Doppler")
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        die("PLAY_SERVICE_ACCOUNT_JSON is not valid JSON")


def access_token(sa):
    """Service-account JWT → OAuth access token, scoped to the publisher API."""
    now = int(time.time())
    header = {"alg": "RS256", "typ": "JWT"}
    claims = {
        "iss": sa["client_email"],
        "scope": "https://www.googleapis.com/auth/androidpublisher",
        "aud": sa["token_uri"],
        "iat": now,
        "exp": now + 3600,
    }
    signing_input = f"{b64u(json.dumps(header).encode())}.{b64u(json.dumps(claims).encode())}"
    with tempfile.NamedTemporaryFile("w", suffix=".pem", delete=False) as f:
        f.write(sa["private_key"])
        key_path = f.name
    try:
        os.chmod(key_path, 0o600)
        sig = subprocess.run(
            ["openssl", "dgst", "-sha256", "-sign", key_path],
            input=signing_input.encode(), capture_output=True, check=True,
        ).stdout
    except subprocess.CalledProcessError as e:
        die(f"openssl could not sign the token: {e.stderr.decode()[:200]}")
    finally:
        os.unlink(key_path)
    body = urllib.parse.urlencode({
        "grant_type": "urn:ietf:params:oauth:grant-type:jwt-bearer",
        "assertion": f"{signing_input}.{b64u(sig)}",
    }).encode()
    try:
        return json.loads(urllib.request.urlopen(sa["token_uri"], body).read())["access_token"]
    except urllib.error.HTTPError as e:
        die(f"token exchange -> {e.code}: {e.read().decode()[:300]}")


class Edit:
    """One Play edit. Use as a context manager: commits on clean exit,
    discards on any exception."""

    def __init__(self, token):
        self.token = token
        self.id = None

    def call(self, method, path, body=None, raw=None, content_type=None, base=API):
        url = f"{base}/edits/{self.id}{path}" if self.id and not path.startswith("/edits") else f"{base}{path}"
        data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
        req = urllib.request.Request(url, data=data, method=method)
        req.add_header("Authorization", f"Bearer {self.token}")
        req.add_header("Content-Type", content_type or "application/json")
        try:
            payload = urllib.request.urlopen(req, timeout=1800).read()
            return json.loads(payload) if payload else {}
        except urllib.error.HTTPError as e:
            raise RuntimeError(f"{method} {url.split('?')[0]} -> {e.code}: {e.read().decode()[:800]}")

    def __enter__(self):
        self.id = self.call("POST", "/edits", {})["id"]
        return self

    def __exit__(self, exc_type, exc, tb):
        if exc_type is None:
            self.call("POST", f"/edits/{self.id}:commit")
            print(f"committed edit {self.id}")
        else:
            try:
                self.call("DELETE", f"/edits/{self.id}")
            except RuntimeError:
                pass
            print(f"discarded edit {self.id}", file=sys.stderr)
        return False


def png_size(path):
    with open(path, "rb") as f:
        head = f.read(24)
    if head[:8] != b"\x89PNG\r\n\x1a\n":
        die(f"{path} is not a PNG")
    return int.from_bytes(head[16:20], "big"), int.from_bytes(head[20:24], "big")


def copy_from_doc():
    """Title, short and full description out of docs/store-listing.md."""
    spec = importlib.util.spec_from_file_location("asc_meta", os.path.join(HERE, "asc-push-metadata.py"))
    meta = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(meta)
    md = open(meta.DOC).read()
    fields = {
        "title": "Pollis",
        "shortDescription": meta.blockquote_after(md, r"### Play short description[^\n]*\n", collapse=True),
        "fullDescription": meta.blockquote_after(md, r"### Full description[^\n]*\n"),
    }
    missing = [k for k, v in fields.items() if not v]
    if missing:
        die(f"could not parse from store-listing.md: {', '.join(missing)}")
    over = [f"{k} is {len(v)} chars, limit {LIMITS[k]}" for k, v in fields.items() if len(v) > LIMITS[k]]
    if over:
        die("copy exceeds Play limits:\n  " + "\n  ".join(over))
    return fields, meta.urls_from_doc()


def cmd_upload(args):
    aab = os.path.abspath(args.aab)
    if not os.path.isfile(aab):
        die(f"{aab} not found — build it first (mobile/CLAUDE.md → Store builds → Android)")
    token = access_token(service_account())
    with Edit(token) as e:
        print(f"uploading {os.path.basename(aab)} ({os.path.getsize(aab) / 1e6:.0f} MB)…")
        bundle = e.call("POST", "/bundles?uploadType=media", raw=open(aab, "rb").read(),
                        content_type="application/octet-stream", base=UPLOAD_API)
        vc = str(bundle["versionCode"])
        print(f"uploaded versionCode {vc}")
        release = {"name": args.name or vc, "versionCodes": [vc], "status": args.status}
        if args.notes:
            release["releaseNotes"] = [{"language": "en-US", "text": args.notes}]
        e.call("PUT", f"/tracks/{args.track}", {"track": args.track, "releases": [release]})
        print(f"track {args.track}: {args.status} release {release['name']}")


def cmd_listing(args):
    fields, urls = copy_from_doc()
    images = {}
    for kind, paths, want in (
        ("icon", [args.icon] if args.icon else [], lambda w, h: (w, h) == (512, 512)),
        ("featureGraphic", [args.feature_graphic] if args.feature_graphic else [], lambda w, h: (w, h) == (1024, 500)),
        ("phoneScreenshots", args.phone or [], lambda w, h: 320 <= min(w, h) and max(w, h) <= 3840 and max(w, h) <= 2 * min(w, h)),
    ):
        for p in paths:
            w, h = png_size(p)
            if not want(w, h):
                die(f"{p} is {w}x{h}, which Play refuses for {kind}")
        if paths:
            images[kind] = paths

    print("Listing (en-US):")
    for k, v in fields.items():
        print(f"  {k:<17} {len(v):>4} chars")
    for kind, paths in images.items():
        print(f"  {kind:<17} {len(paths)} image(s)")
    if args.dry_run:
        print("dry run — nothing sent")
        return

    token = access_token(service_account())
    with Edit(token) as e:
        e.call("PUT", "/listings/en-US", {"language": "en-US", **fields})
        details = {"defaultLanguage": "en-US", "contactEmail": args.contact_email}
        if urls.get("marketingUrl"):
            details["contactWebsite"] = urls["marketingUrl"]
        e.call("PUT", "/details", details)
        for kind, paths in images.items():
            e.call("DELETE", f"/listings/en-US/{kind}")
            for p in paths:
                e.call("POST", f"/listings/en-US/{kind}?uploadType=media", raw=open(p, "rb").read(),
                       content_type="image/png", base=UPLOAD_API)
            print(f"  {kind}: replaced with {len(paths)} image(s)")


def main():
    ap = argparse.ArgumentParser(description="Publish Pollis to Google Play via the Developer API.")
    sub = ap.add_subparsers(dest="cmd", required=True)

    up = sub.add_parser("upload", help="upload an AAB and attach it to a track")
    up.add_argument("aab", nargs="?", default=DEFAULT_AAB)
    up.add_argument("--track", default="alpha", help="internal | alpha (closed) | beta (open) | production")
    up.add_argument("--status", default="draft", help="draft | completed | inProgress | halted")
    up.add_argument("--name", help="release name shown in the console")
    up.add_argument("--notes", help="en-US release notes")
    up.set_defaults(fn=cmd_upload)

    li = sub.add_parser("listing", help="push copy, contact details and images")
    li.add_argument("--icon", help="512x512 PNG")
    li.add_argument("--feature-graphic", help="1024x500 PNG")
    li.add_argument("--phone", nargs="+", help="phone screenshots (2-8), aspect at most 2:1")
    li.add_argument("--contact-email", default="support@pollis.com")
    li.add_argument("--dry-run", action="store_true")
    li.set_defaults(fn=cmd_listing)

    args = ap.parse_args()
    try:
        args.fn(args)
    except RuntimeError as e:
        die(str(e))


if __name__ == "__main__":
    main()
