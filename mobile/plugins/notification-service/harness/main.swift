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
check("attachment placeholder", c.body == "📷 Photo", c.body)

c = run("{\"ok\":true,\"mode\":\"full\",\"items\":[]}")
check("nothing new: generic", c.title == L10n.genericTitle)

c = run("{\"ok\":false,\"reason\":\"lock_busy\"}")
check("failure: original as it arrived", c.title == "New message" && c.body == "You have a new message" && keepsHandle(c))

c = run("not json")
check("garbage: original", c.title == "New message" && keepsHandle(c))

c = run(nil)
check("default stub (internal): original", c.title == "New message")

let noMuted = "{\"conversation_id\":\"c1\",\"sender_name\":\"Ana\",\"body\":\"x\"}"
c = run("{\"ok\":true,\"mode\":\"full\",\"items\":[\(noMuted)]}")
check("an item without `muted` is not shown", c.title == L10n.genericTitle, c.title)

print(failures == 0 ? "all passed" : "\(failures) failed")
exit(failures == 0 ? 0 : 1)
