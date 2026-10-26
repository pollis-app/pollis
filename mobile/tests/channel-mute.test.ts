// #1431: per-channel mute. The mute lives on the DS (it is what skips a muted
// member's push), so the phone's job is to set it, show it, and re-read it when
// another device of the account changes it. These pin the wiring that a device
// run would otherwise be the first to notice broken.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { decodeRealtimeEvent, isTrustedOrigin } from "../lib/realtime/events.ts";

const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");

test("the DS's mute nudge decodes, and only from the DS", () => {
  // The exact shape `pollis-delivery/src/profile.rs` publishes.
  const event = decodeRealtimeEvent(JSON.stringify({ type: "channel_mutes_changed" }));
  assert.ok(event, "channel_mutes_changed must decode");
  assert.equal(event.type, "channel_mutes_changed");
  assert.ok(isTrustedOrigin(event, false));
  assert.ok(!isTrustedOrigin(event, true), "a participant cannot fake the DS nudge");
});

test("the nudge re-reads the account's mutes", () => {
  const inbox = read("../hooks/useInboxRealtime.ts");
  assert.match(
    inbox,
    /case "channel_mutes_changed":[\s\S]*?groupQueryKeys\.mutedChannels\(userId\)/,
  );
});

test("the hooks speak the bridge's argument names", () => {
  // `pollis-core/src/bridge.rs`: list_muted_channels(userId),
  // set_channel_muted(userId, channelId, muted).
  const hooks = read("../hooks/queries/useUserGroups.ts");
  assert.match(hooks, /invoke<string\[\]>\("list_muted_channels", \{ userId: currentUser\.id \}\)/);
  assert.match(
    hooks,
    /invoke\("set_channel_muted", \{\s*userId: currentUser\.id,\s*channelId: vars\.channelId,\s*muted: vars\.muted,\s*\}\)/,
  );
  const bridge = read("../../pollis-core/src/bridge.rs");
  assert.match(bridge, /"set_channel_muted" => \{[\s\S]*?arg\(&args, "userId"\)[\s\S]*?arg\(&args, "channelId"\)[\s\S]*?arg\(&args, "muted"\)/);
});

test("the channel menu carries the toggle, and the list marks a muted channel", () => {
  const sheet = read("../components/chat/ChannelMenuSheet.tsx");
  assert.match(sheet, /testID="toggle-channel-mute"/);
  assert.match(sheet, /useSetChannelMuted\(\)/);
  const panel = read("../components/groups/GroupPanel.tsx");
  // Maestro reads ids, not colours: a muted row's id says so.
  assert.match(panel, /`row-channel-\$\{c\.id\}-muted`/);
  assert.match(panel, /muted=\{isMuted\}/);
});
