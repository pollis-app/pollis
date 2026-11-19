"""Regenerate tests/fixtures/dragons-den: a realistic D&D guild for the importer.

DiscordChatExporter files (copied from ./dnd, with author roles and local emoji
paths), a guild snapshot (discord-guild.json, the REST API's shapes), emoji
images and a server icon. Images are synthetic and drawn here, so the fixture
needs no image tooling.

    python3 gen_dragons_den.py <path to tests/fixtures>
"""

import json
import os
import shutil
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

root = sys.argv[1]
src = os.path.join(root, "dnd")
dst = os.path.join(root, "dragons-den")
shutil.rmtree(dst, ignore_errors=True)
os.makedirs(os.path.join(dst, "emoji"))


from fixture_images import gif, png  # noqa: E402


def d20(color):
    def f(x, y):
        cx, cy = x - 31.5, y - 31.5
        inside = abs(cx) + abs(cy) * 0.9 < 30
        edge = inside and abs(cx) + abs(cy) * 0.9 > 26
        if edge:
            return (20, 20, 20, 255)
        return (*color, 255) if inside else (0, 0, 0, 0)

    return f


png(os.path.join(dst, "icon.png"), 64, 64, lambda x, y: (120 + x, 30, 40 + y, 255))
png(os.path.join(dst, "emoji", "700.png"), 64, 64, d20((250, 191, 90)))
png(os.path.join(dst, "emoji", "702.png"), 64, 64, d20((200, 60, 60)))
png(os.path.join(dst, "emoji", "704.png"), 64, 64, d20((90, 160, 220)))
png(os.path.join(dst, "emoji", "705.png"), 64, 64, d20((60, 60, 60)))
W = 32
frames = [[1 + ((x // 8 + y // 8 + k) % 3) for y in range(W) for x in range(W)] for k in range(4)]
gif(os.path.join(dst, "emoji", "701.gif"), W, W, frames, [(0, 0, 0), (250, 191, 90), (200, 80, 40), (40, 40, 40)])

G = "900000000000000001"
ROLES_BY_AUTHOR = {
    "100": [{"id": "800", "name": "Dungeon Master", "color": "#fabf5a", "position": 4}],
    "101": [
        {"id": "802", "name": "Players", "color": None, "position": 2},
        {"id": "803", "name": "Bard", "color": None, "position": 1},
    ],
    "102": [
        {"id": "802", "name": "Players", "color": None, "position": 2},
        {"id": "801", "name": "Moderator", "color": None, "position": 3},
    ],
}


def copy(name):
    d = json.load(open(os.path.join(src, name)))
    for m in d["messages"]:
        for e in m.get("inlineEmojis", []) + [r["emoji"] for r in m.get("reactions", [])]:
            if e.get("id"):
                ext = "gif" if e["isAnimated"] else "png"
                e["imageUrl"] = f"emoji/{e['id']}.{ext}"
        a = m["author"]
        a["roles"] = ROLES_BY_AUTHOR.get(a["id"], [])
    d["guild"]["iconUrl"] = "icon.png"
    json.dump(d, open(os.path.join(dst, name), "w"), indent=1, ensure_ascii=False)


copy("Dragon's Den - Campaign - session-notes [111].json")
copy("Dragon's Den - Text Channels - general [222].json")
copy("Dragon's Den - Voice - tavern [333].json")
copy("Dragon's Den - session-notes - Loot split [444].json")

VIEW = 1 << 10
SEND = 1 << 11
MANAGE_MSG = 1 << 13
ATTACH = 1 << 15
CONNECT = 1 << 20


def ch(id, t, name, pos, parent=None, topic=None, ow=()):
    return {
        "id": id,
        "type": t,
        "guild_id": G,
        "name": name,
        "position": pos,
        "parent_id": parent,
        "topic": topic,
        "nsfw": False,
        "permission_overwrites": [{"id": o[0], "type": o[1], "allow": str(o[2]), "deny": str(o[3])} for o in ow],
    }


snap = {
    "guild": {
        "id": G,
        "name": "Dragon's Den",
        "icon": "a1b2c3",
        "owner_id": "100",
        "roles": [
            {"id": G, "name": "@everyone", "permissions": str(VIEW | SEND | CONNECT | (1 << 16) | (1 << 6)), "position": 0, "managed": False},
            {"id": "800", "name": "Dungeon Master", "permissions": "8", "position": 4, "managed": False},
            {"id": "801", "name": "Moderator", "permissions": "32", "position": 3, "managed": False},
            {"id": "802", "name": "Players", "permissions": "0", "position": 2, "managed": False},
            {"id": "803", "name": "Bard", "permissions": "0", "position": 1, "managed": False},
            {"id": "804", "name": "Avrae", "permissions": str(SEND), "position": 5, "managed": True},
        ],
        "emojis": [
            {"id": "700", "name": "Nat20", "animated": False},
            {"id": "701", "name": "nat20", "animated": True},
            {"id": "702", "name": "x", "animated": False},
            {"id": "703", "name": "BeholderEye", "animated": False},
            {"id": "704", "name": "Mimic", "animated": False},
            {"id": "705", "name": "crit-fail", "animated": False},
        ],
    },
    "channels": [
        ch("51", 4, "Text Channels", 0),
        ch("50", 4, "Campaign", 1),
        ch("52", 4, "Voice Channels", 2),
        ch("53", 4, "Off-topic", 3),
        ch("54", 4, "Archive", 4),
        ch("220", 5, "rules", 0, None, "Read before your first session"),
        ch("222", 0, "general", 0, "51"),
        ch("223", 0, "looking-for-group", 1, "51", "Find a table"),
        ch("115", 2, "the-table", 0, "50"),
        ch("111", 0, "session-notes", 0, "50", "Recaps after every session"),
        ch("113", 15, "lore", 1, "50", "One post per place, person or plot hook"),
        ch("112", 0, "maps", 2, "50", None, [(G, 0, 0, ATTACH)]),
        ch("114", 0, "gm-only", 3, "50", "Spoilers. Players keep out.", [(G, 0, 0, VIEW), ("800", 0, VIEW | SEND, 0)]),
        ch("333", 2, "tavern", 0, "52"),
        ch("334", 13, "storytime", 1, "52"),
        ch("400", 0, "memes", 0, "53", None, [("102", 1, MANAGE_MSG, 0)]),
        ch("402", 0, "<3-fan-art", 1, "53"),
        ch("401", 0, "general", 2, "53", "Not about the game"),
    ],
    "members": [
        {"user": {"id": "100", "username": "dan", "global_name": "Dan", "bot": False}, "nick": "DM Dan", "roles": ["800"]},
        {"user": {"id": "101", "username": "ashley", "global_name": "Ashley", "bot": False}, "nick": "Ash the Bard", "roles": ["802", "803"]},
        {"user": {"id": "102", "username": "rob", "global_name": None, "bot": False}, "nick": None, "roles": ["802", "801"]},
        {"user": {"id": "103", "username": "Avrae", "global_name": None, "bot": True}, "nick": None, "roles": ["804"]},
        {"user": {"id": "104", "username": "kim", "global_name": "Quiet Kim", "bot": False}, "nick": None, "roles": ["802"]},
    ],
}
json.dump(snap, open(os.path.join(dst, "discord-guild.json"), "w"), indent=1, ensure_ascii=False)
open(os.path.join(dst, "README.txt"), "w").write(
    "A realistic test guild, generated by ../gen_dragons_den.py: DiscordChatExporter files, a guild snapshot (discord-guild.json), emoji images and an icon.\n"
)
