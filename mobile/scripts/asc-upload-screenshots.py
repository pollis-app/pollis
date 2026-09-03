#!/usr/bin/env python3
"""Upload App Store screenshots through the App Store Connect API.

Screenshots are the one release asset with no CLI: `altool`/Transporter move
binaries, `eas submit` needs an EAS account (this repo deliberately has none),
and everything else is drag-and-drop in the ASC web UI. That is fine once and
miserable every release, so this does it from the terminal.

    mobile/scripts/asc-upload-screenshots.py --dry-run .maestro/artifacts/store-*/*.png
    mobile/scripts/asc-upload-screenshots.py --replace .maestro/artifacts/store-*/*.png

Credentials come from Doppler (pollis/prd_prod) unless already in the
environment: ASC_KEY_ID, ASC_ISSUER_ID, ASC_KEY_P8_BASE64. Nothing is printed.

DEPENDENCIES: none. Stdlib plus the `openssl` binary. ASC wants an ES256 JWT,
which normally means PyJWT + cryptography; signing through `openssl dgst`
instead keeps this runnable on a fresh machine, which is exactly when you need
it. The only fiddly part is that OpenSSL emits a DER-wrapped signature and JOSE
wants raw r||s — `_der_to_jose` below is that conversion and nothing more.

The upload itself is a three-step reservation protocol, not a POST of bytes:
reserve the asset (ASC replies with one or more presigned part URLs), PUT each
part, then PATCH `uploaded: true` with an MD5 of the whole file. The checksum is
how ASC decides the asset is intact, so a mismatch fails there rather than
silently shipping a corrupt image.
"""

import argparse
import base64
import glob
import hashlib
import json
import os
import re
import struct
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

API = "https://api.appstoreconnect.apple.com/v1"

# Pixel dimensions -> ASC screenshotDisplayType.
#
# Two traps here, both discovered by asking the API rather than reading the UI:
#
# 1. There is no `APP_IPHONE_69`. App Store Connect's web UI calls the set
#    "iPhone 6.9-inch Display", but the API enum was never renamed and still
#    tops out at `APP_IPHONE_67`. A 1320x2868 (6.9") shot goes into the 6.7"
#    set; they are one set with two accepted sizes, not two sets.
# 2. A display type can be a VALID enum value and still be refused for a given
#    app, with a different error ("Display Type Not Allowed!" rather than "is
#    not a valid value"). That is the API saying the app does not currently
#    claim that device family — which, before any binary is uploaded, is how
#    iPad looks even when `ios.supportsTablet` is true. Upload a build first.
#
# To re-derive the live list, POST an obviously-bogus display type: the 409
# names every accepted value.
DISPLAY_TYPES = {
    (1320, 2868): "APP_IPHONE_67",
    (2868, 1320): "APP_IPHONE_67",
    (1290, 2796): "APP_IPHONE_67",
    (2796, 1290): "APP_IPHONE_67",
    (1242, 2688): "APP_IPHONE_65",
    (1284, 2778): "APP_IPHONE_65",
    (2064, 2752): "APP_IPAD_PRO_3GEN_129",
    (2752, 2064): "APP_IPAD_PRO_3GEN_129",
    (2048, 2732): "APP_IPAD_PRO_3GEN_129",
}


def die(msg):
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(1)


def b64u(raw):
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()


def secret(name):
    """Environment first, then Doppler. Never printed."""
    if os.environ.get(name):
        return os.environ[name]
    try:
        out = subprocess.run(
            ["doppler", "secrets", "get", name, "-p", "pollis", "-c", "prd_prod", "--plain"],
            capture_output=True, text=True, check=True,
        )
    except (subprocess.CalledProcessError, FileNotFoundError):
        die(f"{name} not in the environment and could not be read from Doppler")
    value = out.stdout.strip()
    if not value:
        die(f"{name} is empty")
    return value


def _der_to_jose(der):
    """DER SEQUENCE{INTEGER r, INTEGER s} -> the raw 64-byte r||s JOSE wants."""
    if der[0] != 0x30:
        raise ValueError("signature is not a DER SEQUENCE")
    # Skip the SEQUENCE header (long form when the body exceeds 127 bytes).
    i = 2 if der[1] < 0x80 else 2 + (der[1] & 0x7F)
    out = b""
    for _ in range(2):
        if der[i] != 0x02:
            raise ValueError("expected a DER INTEGER")
        length = der[i + 1]
        value = der[i + 2 : i + 2 + length]
        # DER keeps a leading zero to stay positive; JOSE is fixed-width.
        value = value.lstrip(b"\x00").rjust(32, b"\x00")
        out += value
        i += 2 + length
    return out


def make_token(key_id, issuer_id, p8_pem):
    """ES256 JWT for ASC. 20-minute expiry — Apple rejects anything longer."""
    header = {"alg": "ES256", "kid": key_id, "typ": "JWT"}
    payload = {
        "iss": issuer_id,
        "iat": int(time.time()),
        "exp": int(time.time()) + 20 * 60,
        "aud": "appstoreconnect-v1",
    }
    signing_input = f"{b64u(json.dumps(header).encode())}.{b64u(json.dumps(payload).encode())}"

    with tempfile.NamedTemporaryFile("w", suffix=".pem", delete=False) as f:
        f.write(p8_pem)
        key_path = f.name
    try:
        os.chmod(key_path, 0o600)
        der = subprocess.run(
            ["openssl", "dgst", "-sha256", "-sign", key_path],
            input=signing_input.encode(), capture_output=True, check=True,
        ).stdout
    except subprocess.CalledProcessError as e:
        die(f"openssl could not sign the token: {e.stderr.decode()[:200]}")
    finally:
        os.unlink(key_path)

    return f"{signing_input}.{b64u(_der_to_jose(der))}"


def api(token, method, path, body=None, raw=None, headers=None):
    url = path if path.startswith("http") else f"{API}{path}"
    data = raw if raw is not None else (json.dumps(body).encode() if body else None)
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Authorization", f"Bearer {token}")
    if raw is None and body is not None:
        req.add_header("Content-Type", "application/json")
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req) as resp:
            payload = resp.read()
            return json.loads(payload) if payload and resp.status != 204 else {}
    except urllib.error.HTTPError as e:
        detail = e.read().decode()[:600]
        die(f"{method} {url.split('?')[0]} -> {e.code}\n{detail}")


def png_size(path):
    """Read the IHDR directly — avoids a Pillow dependency for two integers."""
    with open(path, "rb") as f:
        head = f.read(24)
    if head[:8] != b"\x89PNG\r\n\x1a\n":
        die(f"{path} is not a PNG")
    return struct.unpack(">II", head[16:24])


def main():
    ap = argparse.ArgumentParser(description="Upload App Store screenshots via the ASC API.")
    ap.add_argument("files", nargs="+", help="PNG screenshots (globs are expanded)")
    ap.add_argument("--bundle-id", default="com.pollis.mobile")
    ap.add_argument("--locale", default="en-US")
    ap.add_argument("--version", help="version string; default is the editable version")
    ap.add_argument("--platform", default="IOS",
                    help="IOS | MAC_OS | TV_OS — an app can have one version record per platform")
    ap.add_argument("--display-type", help="override the size-derived ASC display type")
    ap.add_argument("--replace", action="store_true",
                    help="delete existing screenshots in each target set first")
    ap.add_argument("--dry-run", action="store_true",
                    help="authenticate and resolve everything, upload nothing")
    args = ap.parse_args()

    paths = sorted({p for pat in args.files for p in (glob.glob(pat) or [pat])})
    missing = [p for p in paths if not os.path.isfile(p)]
    if missing:
        die(f"not found: {', '.join(missing)}")
    if not paths:
        die("no files matched")

    # Group by display type up front so a wrong size fails before any upload.
    by_type = {}
    for p in paths:
        w, h = png_size(p)
        dtype = args.display_type or DISPLAY_TYPES.get((w, h))
        if not dtype:
            die(f"{p} is {w}x{h}, which maps to no known ASC display type; "
                f"pass --display-type explicitly if this is deliberate")
        by_type.setdefault(dtype, []).append((p, w, h))

    print("Planned upload:")
    for dtype, items in by_type.items():
        print(f"  {dtype}")
        for p, w, h in items:
            print(f"    {os.path.basename(p):<34} {w}x{h}  {os.path.getsize(p)/1024:.0f} KB")

    token = make_token(secret("ASC_KEY_ID"), secret("ASC_ISSUER_ID"),
                       base64.b64decode(secret("ASC_KEY_P8_BASE64")).decode())

    apps = api(token, "GET", f"/apps?filter[bundleId]={args.bundle_id}")["data"]
    if not apps:
        die(f"no app with bundleId {args.bundle_id} on this account")
    app_id, app_name = apps[0]["id"], apps[0]["attributes"]["name"]
    print(f"\napp: {app_name} ({args.bundle_id}, id {app_id})")

    versions = api(token, "GET", f"/apps/{app_id}/appStoreVersions?limit=20")["data"]
    # Only a version in an editable state accepts asset changes; a live one does
    # not, and the API's error for that is not obvious.
    editable = {"PREPARE_FOR_SUBMISSION", "DEVELOPER_REJECTED", "REJECTED",
                "METADATA_REJECTED", "WAITING_FOR_REVIEW", "INVALID_BINARY"}
    # Filter by PLATFORM first. An app record can carry a version per platform
    # — this one has both MAC_OS and IOS at 1.0 — and they are indistinguishable
    # by versionString or state. Picking the wrong one does not fail here; it
    # fails much later, when every iPhone display type comes back
    # "Display Type Not Allowed!", which reads like a display-type problem and
    # is really a platform problem.
    candidates = [v for v in versions
                  if v["attributes"].get("platform") == args.platform
                  and (not args.version or v["attributes"]["versionString"] == args.version)
                  and v["attributes"]["appStoreState"] in editable]
    if not candidates:
        states = ", ".join(f'{v["attributes"].get("platform")}/{v["attributes"]["versionString"]}'
                           f'={v["attributes"]["appStoreState"]}' for v in versions) or "none"
        die(f"no editable {args.platform} version found (have: {states}). "
            f"Create one in ASC first, or pass --platform.")
    version = candidates[0]
    version_id = version["id"]
    print(f"version: {version['attributes']['versionString']} "
          f"({args.platform}, {version['attributes']['appStoreState']})")

    locs = api(token, "GET",
               f"/appStoreVersions/{version_id}/appStoreVersionLocalizations"
               f"?filter[locale]={args.locale}")["data"]
    if not locs:
        die(f"no {args.locale} localization on that version; add it in ASC first")
    loc_id = locs[0]["id"]

    sets = api(token, "GET",
               f"/appStoreVersionLocalizations/{loc_id}/appScreenshotSets")["data"]
    existing_sets = {s["attributes"]["screenshotDisplayType"]: s["id"] for s in sets}

    if args.dry_run:
        print(f"\nlocalization {args.locale} ok; existing sets: "
              f"{', '.join(existing_sets) or 'none'}")
        print("dry run — nothing uploaded")
        return

    for dtype, items in by_type.items():
        set_id = existing_sets.get(dtype)
        if not set_id:
            set_id = api(token, "POST", "/appScreenshotSets", {
                "data": {
                    "type": "appScreenshotSets",
                    "attributes": {"screenshotDisplayType": dtype},
                    "relationships": {"appStoreVersionLocalization": {
                        "data": {"type": "appStoreVersionLocalizations", "id": loc_id}}},
                }
            })["data"]["id"]
            print(f"\n{dtype}: created set")
        else:
            print(f"\n{dtype}: using existing set")

        if args.replace:
            for shot in api(token, "GET", f"/appScreenshotSets/{set_id}/appScreenshots")["data"]:
                api(token, "DELETE", f"/appScreenshots/{shot['id']}")
            print("  cleared existing screenshots")

        for path, _w, _h in items:
            blob = open(path, "rb").read()
            name = os.path.basename(path)

            reserved = api(token, "POST", "/appScreenshots", {
                "data": {
                    "type": "appScreenshots",
                    "attributes": {"fileSize": len(blob), "fileName": name},
                    "relationships": {"appScreenshotSet": {
                        "data": {"type": "appScreenshotSets", "id": set_id}}},
                }
            })["data"]
            shot_id = reserved["id"]

            for op in reserved["attributes"]["uploadOperations"]:
                chunk = blob[op["offset"]: op["offset"] + op["length"]]
                hdrs = {h["name"]: h["value"] for h in op.get("requestHeaders", [])}
                req = urllib.request.Request(op["url"], data=chunk, method=op["method"])
                for k, v in hdrs.items():
                    req.add_header(k, v)
                try:
                    urllib.request.urlopen(req).read()
                except urllib.error.HTTPError as e:
                    die(f"uploading {name}: part failed -> {e.code} {e.read().decode()[:200]}")

            api(token, "PATCH", f"/appScreenshots/{shot_id}", {
                "data": {
                    "type": "appScreenshots", "id": shot_id,
                    "attributes": {"uploaded": True,
                                   "sourceFileChecksum": hashlib.md5(blob).hexdigest()},
                }
            })
            print(f"  uploaded {name}")

    # Apple processes asynchronously; a green upload is not yet a valid asset.
    print("\nverifying asset delivery...")
    for dtype in by_type:
        set_id = api(token, "GET",
                     f"/appStoreVersionLocalizations/{loc_id}/appScreenshotSets")["data"]
        set_id = next(s["id"] for s in set_id
                      if s["attributes"]["screenshotDisplayType"] == dtype)
        for shot in api(token, "GET", f"/appScreenshotSets/{set_id}/appScreenshots")["data"]:
            state = shot["attributes"].get("assetDeliveryState") or {}
            errors = state.get("errors") or []
            label = shot["attributes"].get("fileName", shot["id"])
            if errors:
                print(f"  {label}: REJECTED — {errors}")
            else:
                print(f"  {label}: {state.get('state', 'unknown')}")

    print("\nA state of UPLOAD_COMPLETE becomes COMPLETE once Apple finishes "
          "processing; re-run with --dry-run later to re-check.")


if __name__ == "__main__":
    main()
