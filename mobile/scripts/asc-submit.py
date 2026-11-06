#!/usr/bin/env python3
"""Put an uploaded iOS build into App Review (the mobile-release skill's last iOS step).

  mobile/scripts/asc-submit.py <version> <build-number> <notes-file>            plan only, changes nothing
  mobile/scripts/asc-submit.py <version> <build-number> <notes-file> --submit   create/fill the version and submit

Creates the App Store version (or reuses it while still editable), attaches the build,
sets "What's New" on every localization (notes from mobile-release-notes, ≤4000 chars),
and submits it. The release type copies the live version's. Refuses to touch a version
that is already in review or waiting for release. Credentials: ASC_* in Doppler prd_prod,
through asc-upload-screenshots.py's helpers.
"""
import base64, importlib.util, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
HELPER = os.environ.get("ASC_HELPER", os.path.join(HERE, "asc-upload-screenshots.py"))
spec = importlib.util.spec_from_file_location("asc", HELPER)
asc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(asc)

BUNDLE_ID = "com.pollis.ios"
EDITABLE = {"PREPARE_FOR_SUBMISSION", "DEVELOPER_REJECTED", "REJECTED", "METADATA_REJECTED"}
IN_FLIGHT = {"WAITING_FOR_REVIEW", "IN_REVIEW", "PENDING_DEVELOPER_RELEASE", "PROCESSING_FOR_DISTRIBUTION"}


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    submit = "--submit" in sys.argv
    if len(args) != 3:
        asc.die(__doc__)
    version, build_number, notes_path = args
    notes = open(notes_path).read().strip()
    if not notes or len(notes) > 4000:
        asc.die(f"notes must be 1-4000 chars (got {len(notes)})")

    t = asc.make_token(asc.secret("ASC_KEY_ID"), asc.secret("ASC_ISSUER_ID"),
                       base64.b64decode(asc.secret("ASC_KEY_P8_BASE64")).decode())
    g = lambda p: asc.api(t, "GET", p)

    app = g(f"/apps?filter[bundleId]={BUNDLE_ID}")["data"][0]["id"]
    builds = g(f"/builds?filter[app]={app}&filter[version]={build_number}&filter[preReleaseVersion.version]={version}")["data"]
    if not builds:
        asc.die(f"no build {version} ({build_number}) on App Store Connect yet")
    build = builds[0]
    state = build["attributes"]["processingState"]
    print(f"build {version} ({build_number}): {state}, expired={build['attributes']['expired']}")
    if state != "VALID" or build["attributes"]["expired"]:
        asc.die("build is not VALID yet; wait for processing")

    versions = g(f"/apps/{app}/appStoreVersions?filter[platform]=IOS&limit=20")["data"]
    for v in versions:
        print(f"  existing {v['attributes']['versionString']}: {v['attributes']['appStoreState']}")
    busy = [v for v in versions if v["attributes"]["appStoreState"] in IN_FLIGHT]
    if busy:
        asc.die(f"{busy[0]['attributes']['versionString']} is {busy[0]['attributes']['appStoreState']}; not touching it")
    live = next((v for v in versions if v["attributes"]["appStoreState"] == "READY_FOR_SALE"), None)
    release_type = live["attributes"]["releaseType"] if live else "AFTER_APPROVAL"
    mine = next((v for v in versions if v["attributes"]["versionString"] == version), None)
    if mine and mine["attributes"]["appStoreState"] not in EDITABLE:
        asc.die(f"{version} exists in state {mine['attributes']['appStoreState']}")

    print(f"plan: {'reuse' if mine else 'create'} {version}, attach build {build_number}, "
          f"release {release_type}, What's New {len(notes)} chars, then submit")
    if not submit:
        print("plan only; pass --submit to apply")
        return

    if mine:
        vid = mine["id"]
        asc.api(t, "PATCH", f"/appStoreVersions/{vid}/relationships/build",
                {"data": {"type": "builds", "id": build["id"]}})
    else:
        vid = asc.api(t, "POST", "/appStoreVersions", {"data": {
            "type": "appStoreVersions",
            "attributes": {"platform": "IOS", "versionString": version, "releaseType": release_type},
            "relationships": {"app": {"data": {"type": "apps", "id": app}},
                              "build": {"data": {"type": "builds", "id": build["id"]}}}}})["data"]["id"]
    print(f"version {version}: {vid}")

    for loc in g(f"/appStoreVersions/{vid}/appStoreVersionLocalizations")["data"]:
        asc.api(t, "PATCH", f"/appStoreVersionLocalizations/{loc['id']}", {"data": {
            "type": "appStoreVersionLocalizations", "id": loc["id"], "attributes": {"whatsNew": notes}}})
        print(f"  whatsNew set for {loc['attributes']['locale']}")

    sub = asc.api(t, "POST", "/reviewSubmissions", {"data": {
        "type": "reviewSubmissions", "attributes": {"platform": "IOS"},
        "relationships": {"app": {"data": {"type": "apps", "id": app}}}}})["data"]["id"]
    asc.api(t, "POST", "/reviewSubmissionItems", {"data": {
        "type": "reviewSubmissionItems",
        "relationships": {"reviewSubmission": {"data": {"type": "reviewSubmissions", "id": sub}},
                          "appStoreVersion": {"data": {"type": "appStoreVersions", "id": vid}}}}})
    asc.api(t, "PATCH", f"/reviewSubmissions/{sub}", {"data": {
        "type": "reviewSubmissions", "id": sub, "attributes": {"submitted": True}}})
    after = g(f"/appStoreVersions/{vid}")["data"]["attributes"]["appStoreState"]
    print(f"submitted {sub}; {version} is now {after}")


if __name__ == "__main__":
    main()
