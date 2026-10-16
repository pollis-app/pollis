import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { HAPTICS, armHaptic, resolveHaptic, type HapticEvent } from "../lib/hapticEvents.ts";

const src = (p: string) => readFileSync(new URL(`../${p}`, import.meta.url), "utf8");

test("the swipe ticks only when the armed state flips", () => {
  assert.equal(armHaptic(false, false), null);
  assert.equal(armHaptic(true, true), null);
  assert.equal(armHaptic(false, true), "swipeArmed");
  assert.equal(armHaptic(true, false), "swipeDisarmed");
});

test("every event has an iOS and an Android haptic", () => {
  const events: HapticEvent[] = [
    "swipeArmed",
    "swipeDisarmed",
    "doubleTapReply",
    "longPress",
    "reactionAdd",
    "reactionRemove",
    "send",
    "keyTap",
    "toggle",
    "confirmArm",
    "success",
    "destructive",
    "error",
  ];
  assert.deepEqual(Object.keys(HAPTICS).sort(), [...events].sort());
  for (const e of events) {
    assert.ok(HAPTICS[e].ios.kind, e);
    assert.ok(HAPTICS[e].android, e);
  }
});

test("Android plays system haptics, never the raw vibrator", () => {
  const s = src("lib/haptics.ts");
  assert.match(s, /performAndroidHapticsAsync/);
  assert.doesNotMatch(s, /Vibration|vibrate\(/);
  // Every Android constant the table names is mapped.
  for (const { android } of Object.values(HAPTICS)) {
    assert.ok(s.includes(`"${android}"`), android);
  }
});

test("feedback can never throw into the interaction", () => {
  const s = src("lib/haptics.ts");
  assert.match(s, /catch \{\s*return;\s*\}/);
  assert.match(s, /\.catch\(\(\) => \{\}\)/);
});

test("the gesture plays haptics from its worklets via scheduleOnRN", () => {
  const s = src("components/chat/SwipeToReply.tsx");
  assert.match(s, /armHaptic\(armed\.value, next\)/);
  assert.match(s, /scheduleOnRN\(buzz, event\)/);
  assert.match(s, /scheduleOnRN\(buzz, "doubleTapReply"\)/);
});

test("every long press plays a haptic: message rows and every ListRow", () => {
  const row = src("components/chat/MessageRow.tsx");
  assert.match(row, /haptic\("longPress"\)/);
  assert.doesNotMatch(row, /onLongPress=\{onLongPress\}/);
  const ui = src("components/ui.tsx");
  assert.match(ui, /haptic\("longPress"\);\s*onLongPress\(\);/);
  assert.doesNotMatch(ui, /onLongPress=\{disabled \? undefined : onLongPress\}/);
});

test("switches tick, from the knob and from the whole row", () => {
  assert.match(src("components/ui.tsx"), /haptic\("toggle"\);\s*onPress\(\);/);
  assert.match(src("components/self/ToggleRow.tsx"), /haptic\("toggle"\);\s*onToggle\(\);/);
});

test("every PIN keypad key is felt, digits and delete alike", () => {
  assert.match(src("components/auth/PinPad.tsx"), /onPress=\{\(\) => \{\s*haptic\("keyTap"\);/);
});

test("the first tap of every two-tap confirm warns", () => {
  const sites: [string, string][] = [
    ["app/group/invite-links.tsx", "setConfirmRevoke(linkId)"],
    ["app/group/settings.tsx", "setConfirmDeleteGroup(true)"],
    ["app/group/settings.tsx", "setConfirmLeave(true)"],
    ["app/group/settings.tsx", "setConfirmDeleteChannel(channelId)"],
    ["app/group/members.tsx", "setConfirmRemove(memberId)"],
    ["app/group/emoji.tsx", "setConfirmRemove(code)"],
    ["components/groups/GroupMenuSheet.tsx", "setArmed(true)"],
    ["app/dm/info.tsx", "setConfirmLeave(true)"],
    ["app/self/security.tsx", "setConfirmRevoke(deviceId)"],
  ];
  for (const [file, arm] of sites) {
    const s = src(file);
    const i = s.indexOf(arm);
    assert.ok(i > 0, `${file}: ${arm}`);
    assert.match(s.slice(Math.max(0, i - 120), i), /haptic\("confirmArm"\);\s*$/, `${file}: ${arm}`);
  }
});

test("copies confirm, and a failed copy says so", () => {
  for (const f of ["app/chat/[id].tsx", "app/(auth)/emergency-kit.tsx", "components/CreatedInviteLinkCard.tsx"]) {
    assert.match(src(f), /haptic\(copied \? "success" : "error"\)/, f);
  }
});

test("scanning another device's code is felt, and so is signing it in (#1361)", () => {
  const s = src("app/self/link-device.tsx");
  assert.match(s, /haptic\("toggle"\);\s*setStep\(\{ kind: "pin"/);
  assert.match(s, /haptic\("success"\);\s*setStep\(\{ kind: "linked"/);
});

test("mutation haptics play from one MutationCache", () => {
  const qc = src("lib/queryClient.ts");
  assert.match(qc, /new MutationCache\(/);
  assert.match(qc, /mutationCache,/);
  for (const hook of ["onMutate", "onSuccess", "onError"]) {
    assert.match(qc, new RegExp(`${hook}: `), hook);
  }
  assert.equal(resolveHaptic(undefined, {}), null);
  assert.equal(resolveHaptic("success", {}), "success");
  assert.equal(resolveHaptic((v: { verified: boolean }) => (v.verified ? "success" : "toggle"), { verified: false }), "toggle");
});

// A user action that changes something gets feedback. These are the
// mutations that deliberately have none: background or plumbing steps the
// user does not perform as an action (and the preferences writer, whose
// switch already ticked).
const NO_HAPTIC = new Set([
  "useUnlockState",
  "useInitializeIdentity",
  "useLogout",
  "useStartEnrollment",
  "useFetchExportAttachments",
  "useShareExport",
  "usePreferences",
]);

test("every mutation hook declares its haptic, or is a listed exemption", () => {
  const dir = new URL("../hooks/queries/", import.meta.url);
  const missing: string[] = [];
  for (const f of readdirSync(dir).filter((n) => n.endsWith(".ts"))) {
    const s = readFileSync(new URL(f, dir), "utf8");
    const fns = s.split(/(?=export function use)/);
    for (const fn of fns) {
      const name = /export function (use\w+)/.exec(fn)?.[1];
      if (!name || !fn.includes("useMutation(")) {
        continue;
      }
      const body = fn.slice(fn.indexOf("useMutation("));
      const tagged = /^useMutation\(\{\s*\/\/[^\n]*\n\s*meta: \{ haptic:/.test(body);
      if (!tagged && !NO_HAPTIC.has(name)) {
        missing.push(`${f}: ${name}`);
      }
    }
  }
  assert.deepEqual(missing, []);
});

test("the key actions get the right feel", () => {
  const meta = (file: string, hook: string) => {
    const s = src(`hooks/queries/${file}`);
    const i = s.indexOf(`export function ${hook}`);
    return s.slice(s.indexOf("meta:", i), s.indexOf("meta:", i) + 160);
  };
  assert.match(meta("useSafety.ts", "useSetContactVerified"), /vars\.verified \? "success" : "toggle"/);
  assert.match(meta("useBlocks.ts", "useBlockUser"), /success: "destructive"/);
  assert.match(meta("useBlocks.ts", "useReportUser"), /success: "success"/);
  assert.match(meta("useMessages.ts", "useSendMessage"), /mutate: "send"/);
  assert.match(meta("useMessages.ts", "useDeleteMessage"), /success: "destructive"/);
  assert.match(meta("useAuth.ts", "useUnlock"), /success: "success"/);
  assert.match(meta("useReactions.ts", "useToggleReaction"), /"reactionAdd" : "reactionRemove"/);
  assert.match(src("app/self/change-email.tsx"), /meta: \{ haptic: \{ success: "success" \} \}/);
});

test("expo-haptics is a pinned dependency", () => {
  const pkg = JSON.parse(src("package.json"));
  assert.match(pkg.dependencies["expo-haptics"], /^~57\./);
});
