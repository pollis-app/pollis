"""Regenerate tests/fixtures/northwind: a realistic Slack workspace export for the importer.

The standard export an admin downloads (channels.json, groups.json, users.json,
dms.json, mpims.json and per-channel day files, in Slack's own shapes), plus a
workspace snapshot (slack-workspace.json: team.info, emoji.list and
usergroups.list, verbatim shapes) with emoji images and an icon.

What it exercises: public, private and archived channels (one private and
archived); purpose and topic, equal and different; the default channel;
users with and without email, an admin, the primary owner, a guest, a
deactivated account, a bot, an app user and Slackbot; system notices,
threads, a bot message and a file; emoji that collide after normalising
(party-parrot / party_parrot), aliases (to a custom emoji, to a standard
one), a one-character name, a picture missing from the snapshot, an
animated GIF; user groups, one disabled. Every email is @pollis.test.
Images are drawn here, so the fixture needs no image tooling.

    python3 gen_northwind.py <path to tests/fixtures>
"""

import json
import os
import shutil
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from fixture_images import gif, png  # noqa: E402

root = sys.argv[1]
dst = os.path.join(root, "northwind")
shutil.rmtree(dst, ignore_errors=True)
os.makedirs(os.path.join(dst, "emoji"))

TEAM = "T0NW1ND00"


def write(rel, data):
    path = os.path.join(dst, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        json.dump(data, f, indent=2, ensure_ascii=False)
        f.write("\n")


def disc(color, ring=(30, 30, 30)):
    def f(x, y):
        d = ((x - 31.5) ** 2 + (y - 31.5) ** 2) ** 0.5
        if d > 30:
            return (0, 0, 0, 0)
        return (*ring, 255) if d > 26 else (*color, 255)

    return f


def user(uid, name, real, email, display="", **flags):
    return {
        "id": uid,
        "team_id": TEAM,
        "name": name,
        "deleted": flags.get("deleted", False),
        "color": "9f69e7",
        "real_name": real,
        "tz": "Europe/London",
        "profile": {
            "title": "",
            "phone": "",
            "real_name": real,
            "real_name_normalized": real,
            "display_name": display,
            "display_name_normalized": display,
            "email": email,
            "image_72": f"https://avatars.slack-edge.com/{uid}_72.png",
            "team": TEAM,
        },
        "is_admin": flags.get("admin", False),
        "is_owner": flags.get("owner", False),
        "is_primary_owner": flags.get("primary_owner", False),
        "is_restricted": flags.get("guest", False),
        "is_ultra_restricted": False,
        "is_bot": flags.get("bot", False),
        "is_app_user": flags.get("app", False),
        "updated": 1700000000,
    }


USERS = [
    user("U01ANA", "ana", "Ana Lima", "slack-import-ana@pollis.test", "Ana", primary_owner=True, owner=True, admin=True),
    user("U02BEN", "ben", "Ben Okafor", "slack-import-ben@pollis.test", admin=True),
    user("U03CHO", "cho", "Cho Park", "slack-import-cho@pollis.test", "cho"),
    # Email hidden by the workspace's settings, or never set.
    user("U04DEV", "dev", "Dev Raman", ""),
    user("U05EVA", "eva", "Eva Novak", "slack-import-eva@pollis.test", guest=True),
    user("U06FIN", "fin", "Fin Walsh", "slack-import-fin@pollis.test", deleted=True),
    user("U07JO", "jo", "Jo Müller", "slack-import-jo@pollis.test"),
    user("U08GHB", "github", "GitHub", "", bot=True),
    user("U09POLL", "polly", "Polly", "", app=True),
    user("USLACKBOT", "slackbot", "Slackbot", ""),
]
ALL = ["U01ANA", "U02BEN", "U03CHO", "U04DEV", "U05EVA", "U07JO"]


def channel(cid, name, created, purpose, topic, members, archived=False, general=False):
    return {
        "id": cid,
        "name": name,
        "created": created,
        "creator": "U01ANA",
        "is_archived": archived,
        "is_general": general,
        "members": members,
        "pins": [],
        "topic": {"value": topic, "creator": "U01ANA" if topic else "", "last_set": created if topic else 0},
        "purpose": {"value": purpose, "creator": "U01ANA" if purpose else "", "last_set": created if purpose else 0},
    }


CHANNELS = [
    channel("C01RND", "random", 1609459300, "Non-work banter and water cooler conversation", "Non-work banter and water cooler conversation", ALL),
    channel("C02GEN", "general", 1609459200, "Company-wide announcements and work-based matters", "Offsite is 12 March", ALL, general=True),
    channel("C03DES", "design", 1612137600, "Design reviews and critique", "Crit: Thursdays 3pm", ["U01ANA", "U03CHO", "U07JO"]),
    channel("C04ENG", "eng-backend", 1614556800, "", "", ["U01ANA", "U02BEN", "U03CHO", "U06FIN"]),
    channel("C05OLD", "launch-2023", 1672531200, "Coordinating the 2023 launch", "", ["U01ANA", "U02BEN"], archived=True),
    channel("C06EMP", "watercooler-music", 1680000000, "", "", ["U07JO"]),
]
GROUPS = [
    channel("G01LEAD", "leadership", 1609459400, "Leads only", "", ["U01ANA", "U02BEN"]),
    channel("G02HIRE", "hiring-2024", 1704067200, "Interview loop", "", ["U01ANA", "U03CHO"], archived=True),
]
for g in GROUPS:
    g["is_private"] = True

write("users.json", USERS)
write("channels.json", CHANNELS)
write("groups.json", GROUPS)
write("dms.json", [{"id": "D01AB", "created": 1609459500, "members": ["U01ANA", "U02BEN"]}])
write("mpims.json", [{"id": "G9MPIM", "name": "mpdm-ana--ben--cho-1", "created": 1609459600, "creator": "U01ANA", "is_archived": False, "members": ["U01ANA", "U02BEN", "U03CHO"]}])
write("integration_logs.json", {"logs": []})


def msg(ts, user, text, **extra):
    m = {"type": "message", "user": user, "text": text, "ts": ts}
    m.update(extra)
    return m


write("general/2024-01-08.json", [
    msg("1704700800.000100", "U04DEV", "<@U04DEV> has joined the channel", subtype="channel_join"),
    msg("1704704400.000200", "U01ANA", "Welcome everyone :party-parrot: offsite details soon", reactions=[
        {"name": "shipit", "users": ["U02BEN", "U03CHO", "U07JO"], "count": 3},
        {"name": "tada", "users": ["U05EVA"], "count": 1},
    ], pinned_to=["C02GEN"]),
    msg("1704708000.000300", "U02BEN", "agenda draft attached", subtype="file_share", files=[
        {"id": "F01AGENDA", "name": "agenda.pdf", "size": 48213, "url_private": "https://files.slack.com/files-pri/T0NW1ND00-F01AGENDA/agenda.pdf"},
    ]),
    msg("1704711600.000400", "U03CHO", "can we do a thread for travel?", thread_ts="1704708000.000300", parent_user_id="U02BEN"),
])
write("general/2024-01-09.json", [
    msg("1704790800.000100", "U07JO", "Travel booked :partyparrot: :party-parrot:"),
    msg("1704794400.000200", "U01ANA", "set the channel topic: Offsite is 12 March", subtype="channel_topic"),
])
write("design/2024-02-01.json", [
    msg("1706781600.000100", "U03CHO", "new logo options :nw-logo:", reactions=[{"name": "party_parrot", "users": ["U07JO"], "count": 1}]),
    msg("1706785200.000200", "U07JO", "option B :ship-it:", edited={"user": "U07JO", "ts": "1706785300.000000"}),
])
write("eng-backend/2024-01-09.json", [
    msg("1704790000.000100", "U02BEN", "deploy at 4"),
    {"type": "message", "subtype": "bot_message", "bot_id": "B01GH", "username": "github", "text": "PR #12 merged", "ts": "1704791000.000200"},
    msg("1704792000.000300", "U06FIN", "last day today, thanks all"),
])
write("launch-2023/2023-05-02.json", [
    msg("1683021600.000100", "U01ANA", "launch is go :shipit:"),
])
write("leadership/2024-03-01.json", [
    msg("1709287200.000100", "U02BEN", "budget review Friday"),
])
write("hiring-2024/2024-04-02.json", [
    msg("1712052000.000100", "U03CHO", "two candidates for Thursday"),
])

# The workspace snapshot: what a standard export lacks.
EMOJI_URL = "https://emoji.slack-edge.com/T0NW1ND00/{}/abc123.{}"
emoji = {
    "party-parrot": EMOJI_URL.format("party-parrot", "gif"),
    "party_parrot": EMOJI_URL.format("party_parrot", "png"),
    "partyparrot": "alias:party-parrot",
    "shipit": EMOJI_URL.format("shipit", "png"),
    "ship-it": "alias:shipit",
    "thumbsup_all": "alias:thumbsup",
    "x": EMOJI_URL.format("x", "png"),
    "nw-logo": EMOJI_URL.format("nw-logo", "png"),
    "missing-pic": EMOJI_URL.format("missing-pic", "png"),
}
write("slack-workspace.json", {
    "team": {
        "id": TEAM,
        "name": "Northwind Studio",
        "domain": "northwind-studio",
        "email_domain": "",
        "icon": {"image_132": "https://avatars.slack-edge.com/northwind_132.png", "image_default": False},
    },
    "emoji": emoji,
    "usergroups": [
        {
            "id": "S01DES",
            "team_id": TEAM,
            "is_usergroup": True,
            "name": "Design team",
            "description": "Everyone who reviews design",
            "handle": "design-team",
            "date_delete": 0,
            "users": ["U03CHO", "U07JO", "U06FIN"],
            "user_count": 3,
        },
        {
            "id": "S02OLD",
            "team_id": TEAM,
            "is_usergroup": True,
            "name": "Launch crew",
            "description": "",
            "handle": "launch-crew",
            "date_delete": 1700000000,
            "users": [],
            "user_count": 0,
        },
    ],
})

png(os.path.join(dst, "icon.png"), 64, 64, lambda x, y: (20 + x, 90 + y, 160, 255))
png(os.path.join(dst, "emoji", "party_parrot.png"), 64, 64, disc((230, 90, 60)))
png(os.path.join(dst, "emoji", "shipit.png"), 64, 64, disc((40, 160, 90)))
png(os.path.join(dst, "emoji", "x.png"), 64, 64, disc((200, 40, 40)))
png(os.path.join(dst, "emoji", "nw-logo.png"), 64, 64, disc((40, 90, 200), ring=(250, 250, 250)))
W = 32
frames = [[1 + ((x // 8 + y // 8 + k) % 3) for y in range(W) for x in range(W)] for k in range(4)]
gif(os.path.join(dst, "emoji", "party-parrot.gif"), W, W, frames, [(0, 0, 0), (240, 60, 200), (60, 200, 240), (250, 220, 40)])
