// macOS harness for NotificationService.swift (#1599): runs the real rewrite against canned
// pollis_notify_fetch results. `xcrun simctl push` posts straight to the notification center and
// never starts an extension, so this is the tier that exercises the Swift. Run: ./run.sh
import Foundation
import UserNotifications

var failures = 0

func check(_ name: String, _ ok: Bool, _ detail: String = "") {
  print("\(ok ? "ok  " : "FAIL") \(name)\(detail.isEmpty ? "" : " — \(detail)")")
  if !ok {
    failures += 1
  }
}

func original() -> UNNotificationContent {
  let c = UNMutableNotificationContent()
  c.title = "New message"
  c.body = "You have a new message"
  c.userInfo = ["body": ["h": "handle-1"], "aps": ["mutable-content": 1]]
  return c
}

func run(_ fixture: String?) -> UNNotificationContent {
  if let fixture {
    setenv("NSE_FIXTURE", fixture, 1)
  } else {
    unsetenv("NSE_FIXTURE")
  }
  let result = PollisNotifyResult.fetch(dataDir: "/tmp/none", handle: "handle-1", budgetMs: 100)
  var out: UNNotificationContent?
  let done = DispatchSemaphore(value: 0)
  NotificationService.rewrite(original(), with: result) { content in
    out = content
    done.signal()
  }
  _ = done.wait(timeout: .now() + 10)
  return out ?? UNNotificationContent()
}

func item(_ extra: String) -> String {
  "{\"conversation_id\":\"c1\",\"message_id\":\"m1\",\"sender_name\":\"Ana\",\"conversation_title\":null,\"is_dm\":true,\"body\":\"hi there\",\"body_key\":null,\"muted\":false\(extra)}"
}

let keepsHandle: (UNNotificationContent) -> Bool = { c in
  ((c.userInfo["body"] as? [String: Any])?["h"] as? String) == "handle-1"
}

check("handle is read from userInfo.body.h", NotificationService.handle(of: original().userInfo) == "handle-1")
check("no body → no handle", NotificationService.handle(of: [:]) == nil)

setenv("NSE_FIXTURE", "ECHO_HANDLE", 1)
let echoed = PollisNotifyResult.fetch(dataDir: "/tmp/none", handle: "handle-1", budgetMs: 1)
check("the handle reaches the core", echoed?.reason == "handle-1", echoed?.reason ?? "nil")

var c = run("{\"ok\":true,\"mode\":\"full\",\"items\":[\(item(""))]}")
check("full DM: sender title", c.title == "Ana", c.title)
check("full DM: preview body", c.body == "hi there", c.body)
check("full DM: thread = conversation", c.threadIdentifier == "c1", c.threadIdentifier)
check("full DM: routing data untouched", keepsHandle(c))

let group = "{\"conversation_id\":\"g1\",\"message_id\":\"m2\",\"sender_name\":\"Ana\",\"conversation_title\":\"Team\",\"is_dm\":false,\"body\":\"standup\",\"muted\":false}"
c = run("{\"ok\":true,\"mode\":\"full\",\"items\":[\(group)]}")
check("strings are localised, not keys", L10n.genericTitle == "New message", L10n.genericTitle)
check("group: sender · conversation", c.title == "Ana · Team", c.title)
check("group: thread", c.threadIdentifier == "g1")

c = run("{\"ok\":true,\"mode\":\"name\",\"items\":[\(item(""))]}")
check("name only: sender title", c.title == "Ana")
check("name only: no preview", c.body == L10n.nameOnlyBody && c.body != "hi there", c.body)

c = run("{\"ok\":true,\"mode\":\"none\",\"items\":[\(item(""))]}")
check("nothing: generic", c.title == "New message" && c.body == "You have a new message", "\(c.title) / \(c.body)")
check("nothing: no thread leak", c.threadIdentifier.isEmpty)

let muted = item("").replacingOccurrences(of: "\"muted\":false", with: "\"muted\":true")
c = run("{\"ok\":true,\"mode\":\"full\",\"items\":[\(muted)]}")
check("all muted: generic, nothing named", c.title == L10n.genericTitle && !c.body.contains("hi"), c.title)

c = run("{\"ok\":true,\"mode\":\"full\",\"items\":[\(group),\(muted)]}")
check("muted newest is skipped for an unmuted one", c.title.contains("Ana") && c.threadIdentifier == "g1", c.threadIdentifier)

let image = item("").replacingOccurrences(of: "\"body_key\":null", with: "\"body_key\":\"image\"")
c = run("{\"ok\":true,\"mode\":\"full\",\"items\":[\(image)]}")
check("attachment placeholder", c.body == "Photo", c.body)

c = run("{\"ok\":true,\"mode\":\"full\",\"items\":[]}")
check("nothing new: generic", c.title == L10n.genericTitle)

c = run("{\"ok\":true,\"mode\":\"none\",\"items\":[]}")
check("no escrow yet (none, empty): generic", c.title == "New message" && c.threadIdentifier.isEmpty)

let mention = item(",\"mention\":true")
c = run("{\"ok\":true,\"mode\":\"full\",\"items\":[\(mention)]}")
check("a mention (muted:false, extra field) is shown", c.title == "Ana" && c.body == "hi there", c.title)

c = run("{\"ok\":false,\"reason\":\"lock_busy\"}")
check("failure: original as it arrived", c.title == "New message" && c.body == "You have a new message" && keepsHandle(c))

c = run("not json")
check("garbage: original", c.title == "New message" && keepsHandle(c))

c = run(nil)
check("default stub (internal): original", c.title == "New message")

let noMuted = "{\"conversation_id\":\"c1\",\"sender_name\":\"Ana\",\"body\":\"x\"}"
c = run("{\"ok\":true,\"mode\":\"full\",\"items\":[\(noMuted)]}")
check("an item without `muted` is not shown", c.title == L10n.genericTitle, c.title)

// Letter avatars (Android's LetterAvatar.kt pins the same numbers).
let defaults = LetterAvatar.colors(accent: LetterAvatar.defaultAccent, background: LetterAvatar.defaultBackground)
check("avatar fill = semantic.high", defaults.fill == 0x332C20, String(defaults.fill, radix: 16))
check("avatar letter = semantic.dim", defaults.letter == 0xC8B698, String(defaults.letter, radix: 16))
check("initial: first character, upper-cased", LetterAvatar.initial("  ana") == "A")
check("initial: an emoji stays whole", LetterAvatar.initial("😀 smile") == "😀")
check("initial: blank has none", LetterAvatar.initial("   ") == nil)
check("hex parses", LetterAvatar.parseHex("#fabf5a") == 0xFABF5A && LetterAvatar.parseHex("nope") == nil)
let suite = UserDefaults(suiteName: "pollis.harness.\(getpid())")
check("no saved theme: defaults", LetterAvatar.theme(suite) == (LetterAvatar.defaultAccent, LetterAvatar.defaultBackground))
suite?.set("#5ad1fa", forKey: LetterAvatar.accentKey)
suite?.set("#101010", forKey: LetterAvatar.backgroundKey)
check("saved theme is read", LetterAvatar.theme(suite) == (0x5AD1FA, 0x101010))
let png = LetterAvatar.png(label: "Ana", rounded: false, theme: (LetterAvatar.defaultAccent, LetterAvatar.defaultBackground))
check("avatar renders a PNG", png?.starts(with: [0x89, 0x50, 0x4E, 0x47]) == true, "\(png?.count ?? 0) bytes")
// NSE_AVATAR_OUT=<dir> writes the two shapes, to look at.
if let dir = ProcessInfo.processInfo.environment["NSE_AVATAR_OUT"] {
  try? png?.write(to: URL(fileURLWithPath: dir).appendingPathComponent("person.png"))
  try? LetterAvatar.png(label: "Team", rounded: true, theme: (LetterAvatar.defaultAccent, LetterAvatar.defaultBackground))?
    .write(to: URL(fileURLWithPath: dir).appendingPathComponent("group.png"))
}
check("no letter, no avatar", LetterAvatar.png(label: " ", rounded: true, theme: (0, 0)) == nil)

// The intent is a combination iOS donates: sender, recipients, conversation id; images set.
let theme = (accent: LetterAvatar.defaultAccent, background: LetterAvatar.defaultBackground)
let dmItem = try! JSONDecoder().decode(PollisNotifyItem.self, from: Data(item("").utf8))
let dmIntent = NotificationService.messageIntent(for: dmItem, groupName: nil, theme: theme)
check("DM intent: me as the one recipient", dmIntent.recipients?.count == 1 && dmIntent.recipients?.first?.isMe == true)
check("DM intent: sender with avatar", dmIntent.sender?.displayName == "Ana" && dmIntent.sender?.image != nil)
check("DM intent: conversation id, no group", dmIntent.conversationIdentifier == "c1" && dmIntent.speakableGroupName == nil)
let groupItem = try! JSONDecoder().decode(PollisNotifyItem.self, from: Data(group.utf8))
let groupIntent = NotificationService.messageIntent(for: groupItem, groupName: "Team", theme: theme)
check("group intent: two recipients", groupIntent.recipients?.count == 2)
// The group image is set with setImage(forParameterNamed:), iOS-only API; here, its PNG renders.
check("group intent: named", groupIntent.speakableGroupName?.spokenPhrase == "Team")
check("group avatar renders", LetterAvatar.png(label: "Team", rounded: true, theme: theme) != nil)
check("group intent: conversation id", groupIntent.conversationIdentifier == "g1")

check("peak footprint is measured", PollisShared.peakFootprintKB() > 0, "\(PollisShared.peakFootprintKB()) KB")

print(failures == 0 ? "all passed" : "\(failures) failed")
exit(failures == 0 ? 0 : 1)
