#!/usr/bin/env python3
"""What changed for phone users since the version a store last approved.

    mobile/scripts/release-changes.py ios|android [--to REF] [--json]

1. Baseline: the newest version that passed review.
   - iOS: the highest App Store version App Review approved (live, waiting for
     release, or replaced by a later one).
   - Android: the highest versionCode in a Play production release that went
     out (completed, or rolling out).
2. Its commit, from mobile/store/runtime-versions.json: the ledger records the
   commit every store binary was built from (channel app-store / play).
3. Every first-parent commit on REF (default origin/main) since then that
   touches what a phone runs: mobile/, or a crate the app links (pollis-core,
   pollis-api; their changes may still be desktop-only, e.g. behind `media`).
4. For each, its PR (number from the squash subject), title, labels, body and
   the top-level areas it touched, so the notes writer can tell a phone change
   from a desktop one.

Read-only: it changes nothing in either store. The mobile-release-notes skill
turns the output into user-facing copy.
"""

import argparse
import base64
import importlib.util
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
LEDGER = os.path.join(REPO, "mobile", "store", "runtime-versions.json")
PHONE_PATHS = ["mobile/", "pollis-core/", "pollis-api/"]
BOOKKEEPING = re.compile(r"^(mobile/store/runtime-versions\.json|mobile/package\.json|mobile/store/release-notes/.*)$")
# Files that never reach a phone: tests, Maestro flows, docs, CI.
NOT_SHIPPED = re.compile(r"(^|/)(tests?|\.maestro|\.codesight|docs|\.github)/|\.md$|^mobile/scripts/")
# App Review approved these; anything else (in review, rejected, being prepared) did not pass.
APPROVED_IOS = {
    "READY_FOR_SALE",
    "READY_FOR_DISTRIBUTION",
    "PENDING_DEVELOPER_RELEASE",
    "PENDING_APPLE_RELEASE",
    "PROCESSING_FOR_DISTRIBUTION",
    "REPLACED_WITH_NEW_VERSION",
    "REMOVED_FROM_SALE",
    "DEVELOPER_REMOVED_FROM_SALE",
}


def die(msg):
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(1)


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), os.path.join(HERE, name + ".py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def vkey(v):
    return tuple(int(p) for p in v.split("."))


def version_of_code(code):
    # Inverse of app.config.js: ((major*100+minor)*100+patch)*100+build.
    code //= 100
    return f"{code // 10000}.{code // 100 % 100}.{code % 100}"


def ios_baseline():
    asc = load("asc-upload-screenshots")
    t = asc.make_token(asc.secret("ASC_KEY_ID"), asc.secret("ASC_ISSUER_ID"),
                       base64.b64decode(asc.secret("ASC_KEY_P8_BASE64")).decode())
    app = asc.api(t, "GET", "/apps?filter[bundleId]=com.pollis.ios")["data"][0]["id"]
    versions = asc.api(t, "GET", f"/apps/{app}/appStoreVersions?filter[platform]=IOS&limit=50")["data"]
    passed = [v["attributes"]["versionString"] for v in versions if v["attributes"]["appStoreState"] in APPROVED_IOS]
    if not passed:
        die("no iOS version has passed App Review yet")
    return max(passed, key=vkey)


def android_baseline():
    pp = load("play-publish")
    e = pp.Edit(pp.access_token(pp.service_account()))
    e.__enter__()
    try:
        releases = e.call("GET", "/tracks/production").get("releases", [])
    finally:
        e._discard()
    codes = [int(c) for r in releases if r.get("status") in ("completed", "inProgress") for c in r.get("versionCodes", [])]
    if not codes:
        die("Play production has no release that went out")
    return version_of_code(max(codes))


def ledger_commit(platform, version):
    channel = "app-store" if platform == "ios" else "play"
    builds = json.load(open(LEDGER))["builds"]
    hits = [b for b in builds if b["platform"] == platform and b["channel"] == channel and b["version"] == version]
    if not hits:
        die(f"{platform} {version} is not in {os.path.relpath(LEDGER, REPO)}; record it first")
    return hits[-1]["commit"]


def git(*args):
    return subprocess.run(["git", "-C", REPO, *args], check=True, capture_output=True, text=True).stdout


def pr_details(number):
    out = subprocess.run(["gh", "pr", "view", str(number), "--json", "title,body,labels,url"],
                         cwd=REPO, capture_output=True, text=True)
    if out.returncode != 0:
        return None
    d = json.loads(out.stdout)
    body = re.sub(r"🤖 Generated with.*", "", d.get("body") or "", flags=re.S).strip()
    return {"title": d["title"], "labels": [l["name"] for l in d.get("labels", [])], "body": body[:1500], "url": d["url"]}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("platform", choices=["ios", "android"])
    ap.add_argument("--to", default="origin/main", help="the release commit (default origin/main)")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    baseline = ios_baseline() if args.platform == "ios" else android_baseline()
    base = ledger_commit(args.platform, baseline)
    to = git("rev-parse", args.to).strip()
    log = git("log", "--first-parent", "--format=%H%x09%s", f"{base}..{to}", "--", *PHONE_PATHS)
    changes, skipped = [], 0
    for line in log.splitlines():
        sha, subject = line.split("\t", 1)
        files = git("diff-tree", "--no-commit-id", "--name-only", "-r", sha).split()
        # Release bookkeeping (ledger entries, version bumps) is never a user-facing change.
        if all(BOOKKEEPING.match(f) for f in files):
            skipped += 1
            continue
        areas = sorted({f.split("/")[0] for f in files})
        m = re.findall(r"\(#(\d+)\)", subject)
        pr = int(m[-1]) if m else None
        changes.append({"commit": sha[:8], "subject": subject, "areas": areas, "pr": pr,
                        "tests_or_docs_only": all(NOT_SHIPPED.search(f) for f in files),
                        **({"details": pr_details(pr)} if pr else {})})

    result = {"platform": args.platform, "baseline": baseline, "baseline_commit": base, "to": to,
              "changes": changes, "bookkeeping_skipped": skipped}
    if args.json:
        print(json.dumps(result, indent=2))
        return
    print(f"{args.platform}: last version through review {baseline} ({base[:8]}), release commit {to[:8]}")
    print(f"{len(changes)} change(s) touching {', '.join(PHONE_PATHS)} ({skipped} bookkeeping commit(s) skipped)\n")
    for c in changes:
        flag = "  (tests/docs only)" if c["tests_or_docs_only"] else ""
        print(f"- {c['subject']}  [{', '.join(c['areas'])}]{flag}")
        d = c.get("details")
        if d:
            if d["labels"]:
                print(f"    labels: {', '.join(d['labels'])}")
            first = d["body"].split("\n\n")[0].replace("\n", " ")
            if first:
                print(f"    {first[:400]}")


if __name__ == "__main__":
    main()
