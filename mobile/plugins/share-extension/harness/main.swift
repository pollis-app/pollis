// macOS harness for the share extension's logic (#2100): the real ShareModel, ShareCore wrappers,
// intake rules and copy, against a stub core (stub.c). A simulator cannot be driven into a share
// extension from the command line, so this is the tier that exercises the Swift. Run: ./run.sh
import Foundation
import UniformTypeIdentifiers

var failures = 0

func check(_ name: String, _ ok: Bool, _ detail: String = "") {
  print("\(ok ? "ok  " : "FAIL") \(name)\(detail.isEmpty ? "" : " — \(detail)")")
  if !ok {
    failures += 1
  }
}

/// A model whose work, main and debounce all run inline, so each case reads the result at once.
func model(biometric: @escaping (String) -> BiometricRead = { _ in .cancelled }) -> ShareModel {
  ShareModel(
    core: PollisShareCore(),
    work: { $0() },
    main: { $0() },
    after: { _, run in run() },
    debounce: 0,
    readBiometric: biometric
  )
}

func textOnly(_ s: String = "hello") -> SharedContent {
  var c = SharedContent()
  c.texts = [s]
  return c
}

func unlockedModel(_ content: SharedContent = textOnly()) -> ShareModel {
  stub_reset()
  setenv("SHARE_STATE", "locked", 1)
  let m = model()
  m.start(content: content, dataDir: "/tmp/none")
  for d in "1234" {
    m.typeDigit(d)
  }
  return m
}

// MARK: Copy

check("strings load from the generated table", ShareStrings.t("title") == "Send to", ShareStrings.t("title"))
check("one attempt is singular", ShareStrings.plural("pinIncorrect", 1) == "Wrong PIN. 1 attempt remaining.")
check("several attempts are plural", ShareStrings.plural("pinIncorrect", 7) == "Wrong PIN. 7 attempts remaining.")
check("a placeholder is filled", ShareStrings.t("channelIn", "Book club") == "in Book club")

// MARK: ULID

let ulid = ULID.make()
check("a share id is a 26-character ULID", ulid.count == 26 && ulid.allSatisfy { "0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains($0) }, ulid)
check("a ULID starts with its time", ULID.encode([UInt8](repeating: 0, count: 16)) == String(repeating: "0", count: 26))
check("all ones encodes as the maximum", ULID.encode([UInt8](repeating: 0xFF, count: 16)) == "7" + String(repeating: "Z", count: 25))
let earlier = ULID.make(date: Date(timeIntervalSince1970: 1_700_000_000))
let later = ULID.make(date: Date(timeIntervalSince1970: 1_800_000_000))
check("ULIDs sort by time", earlier < later)
// The ULID spec's example: 1469922850259 ms is 01ARZ3NDEK.
let spec = ULID.make(date: Date(timeIntervalSince1970: 1_469_922_850.259 + 0.0005))
check("the time prefix matches the spec", spec.hasPrefix("01ARZ3NDEK"), spec)

// MARK: Opening

stub_reset()
setenv("SHARE_STATE", "signed_out", 1)
var m = model()
m.start(content: textOnly(), dataDir: "/tmp/none")
check("signed out shows the sign-in notice",
      m.phase == .notice(title: "Sign in to Pollis first", body: ShareStrings.t("signedOutBody")))
m.cancel()
check("cancel closes the session", stub_close_count() == 1)

stub_reset()
m = model()
m.start(content: textOnly(), dataDir: nil)
check("no App Group is signed out too", m.phase == .notice(title: "Sign in to Pollis first", body: ShareStrings.t("signedOutBody")))

stub_reset()
setenv("SHARE_STATE", "locked_out", 1)
m = model()
m.start(content: textOnly(), dataDir: "/tmp/none")
check("a locked-out account sends the user to the app", m.phase == .notice(title: ShareStrings.t("lockedOutTitle"), body: ShareStrings.t("lockedOutBody")))

stub_reset()
setenv("SHARE_STATE", "locked", 1)
m = model()
m.start(content: SharedContent(), dataDir: "/tmp/none")
check("nothing to send never opens the session", m.phase == .notice(title: ShareStrings.t("nothingToSend"), body: "") && stub_close_count() == 0)

// MARK: PIN, the app's counter

stub_reset()
setenv("SHARE_STATE", "locked", 1)
m = model()
m.start(content: textOnly(), dataDir: "/tmp/none")
check("signed in opens on the keypad", m.phase == .locked)
for d in "9999" {
  m.typeDigit(d)
}
check("a wrong PIN counts down", m.pinError == "Wrong PIN. 9 attempts remaining." && m.pin.isEmpty, m.pinError ?? "nil")
m.typeDigit("x")
check("a non-digit is ignored", m.pin.isEmpty)
m.typeDigit("1")
m.deleteDigit()
check("delete removes a digit", m.pin.isEmpty)
for _ in 0..<8 {
  for d in "0000" {
    m.typeDigit(d)
  }
}
check("the ninth miss leaves one attempt", m.pinError == "Wrong PIN. 1 attempt remaining.", m.pinError ?? "nil")
for d in "0000" {
  m.typeDigit(d)
}
check("the tenth locks the account out", m.phase == .notice(title: ShareStrings.t("lockedOutTitle"), body: ShareStrings.t("lockedOutBody")))
check("the stub counted every attempt", stub_attempts() == 0)

m = unlockedModel()
check("the right PIN opens the picker", m.phase == .picking)
check("the picker lists every target, the Vault first", m.targets.map(\.id) == ["vault", "d1", "c1", "d2", "d3", "d4"])
check("the Vault is recognised", m.targets.first?.isVault == true)
check("shared text pre-fills the message", m.message == "hello")

// MARK: Biometrics

stub_reset()
setenv("SHARE_STATE", "locked_bio", 1)
m = model(biometric: { _ in .key("good") })
m.start(content: textOnly(), dataDir: "/tmp/none")
check("biometrics unlock at once when on", m.phase == .picking)
check("a biometric unlock spends no attempt", stub_attempts() == 10)

stub_reset()
m = model(biometric: { _ in .cancelled })
m.start(content: textOnly(), dataDir: "/tmp/none")
check("a cancelled Face ID falls back to the keypad", m.phase == .locked && !m.pinBusy && m.pinError == nil)
check("a cancel spends no attempt", stub_attempts() == 10)

stub_reset()
m = model(biometric: { _ in .key("stale") })
m.start(content: textOnly(), dataDir: "/tmp/none")
check("a key the core refuses shows the changed note", m.phase == .locked && m.pinError == ShareStrings.t("biometricChanged"))

stub_reset()
m = model(biometric: { _ in .missing })
m.start(content: textOnly(), dataDir: "/tmp/none")
check("a missing key shows the changed note", m.phase == .locked && m.pinError == ShareStrings.t("biometricChanged"))
for d in "1234" {
  m.typeDigit(d)
}
check("the PIN still works after", m.phase == .picking)

stub_reset()
setenv("SHARE_STATE", "locked", 1)
m = model(biometric: { _ in .key("good") })
m.start(content: textOnly(), dataDir: "/tmp/none")
check("biometrics are not tried when off", m.phase == .locked)

// MARK: Picker

m = unlockedModel()
for t in m.targets.prefix(5) {
  m.toggle(t)
}
check("five chats can be chosen, the Vault counting", m.selected.count == 5 && m.selected.first?.isVault == true)
m.toggle(m.targets[5])
check("a sixth is refused with the note", m.selected.count == 5 && m.capReached)
m.toggle(m.targets[0])
check("removing one clears the note", m.selected.count == 4 && !m.capReached)
m.toggle(m.targets[5])
check("and makes room again", m.selected.count == 5)

m.search("zzz")
check("a search with no match is empty", m.targets.isEmpty && m.query == "zzz")
m.search("net")
check("a failed load offers a retry", m.targetsError == ShareStrings.t("loadFailed"))
m.search("")
check("clearing the search lists everything again", m.targets.count == 6 && m.targetsError == nil)

// MARK: Sending

m = unlockedModel()
check("nothing chosen, nothing to send", !m.canSend)
m.toggle(m.targets[1])
m.toggle(m.targets[2])
m.message = "  look  "
var finishedWith: Bool?
m.onFinish = { finishedWith = $0 }
m.send()
check("one send reaches the core", stub_send_count() == 1)
check("the share id travels", String(cString: stub_last_share_id()) == m.shareId)
check("each target has its outcome", m.outcomes == ["d1": "sent", "c1": "queued"], "\(m.outcomes)")
check("a queued target waits on Done", m.phase == .finished && finishedWith == nil && !m.sending)
m.send()
check("Send cannot run twice", stub_send_count() == 1)
m.done()
check("Done closes and completes", finishedWith == false && stub_close_count() == 1)
m.done()
check("and only once", stub_close_count() == 1)

m = unlockedModel()
setenv("SHARE_ALL_SENT", "1", 1)
m.toggle(m.targets[1])
finishedWith = nil
m.onFinish = { finishedWith = $0 }
m.send()
unsetenv("SHARE_ALL_SENT")
check("all sent completes at once", finishedWith == false && stub_close_count() == 1)

m = unlockedModel()
m.toggle(m.targets[1])
setenv("SHARE_SEND_FAIL", "1", 1)
m.send()
unsetenv("SHARE_SEND_FAIL")
check("a failed send says so and stays", m.sendError == ShareStrings.t("sendFailed") && m.phase == .picking && !m.sending)
let firstId = String(cString: stub_last_share_id())
m.send()
check("a retry reuses the share id", String(cString: stub_last_share_id()) == firstId && stub_send_count() == 2)

// MARK: Files

let dir = FileManager.default.temporaryDirectory.appendingPathComponent("pollis-share-harness-\(UUID().uuidString)")
try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
func tempFile(_ name: String, bytes: Int) -> ShareFile {
  let url = dir.appendingPathComponent(name)
  FileManager.default.createFile(atPath: url.path, contents: Data(count: bytes))
  return ShareFile(url: url, filename: name, contentType: "application/octet-stream", kind: .file, size: Int64(bytes))
}

var content = SharedContent()
content.add(tempFile("big.bin", bytes: 0))
let huge = ShareFile(url: dir.appendingPathComponent("huge.mov"), filename: "huge.mov", contentType: "video/mp4", kind: .video, size: shareMaxFileBytes + 1)
FileManager.default.createFile(atPath: huge.url.path, contents: Data())
content.add(huge)
check("a file over 100 MB is refused and deleted", content.tooLarge == ["huge.mov"] && !FileManager.default.fileExists(atPath: huge.url.path))
check("and the share says so", content.issue == ShareStrings.t("tooLarge", "huge.mov"))
var many = SharedContent()
for i in 0..<11 {
  many.add(tempFile("f\(i)", bytes: 1))
}
check("an eleventh file is refused", many.files.count == 10 && many.tooMany && many.issue == ShareStrings.t("tooMany"))
check("text and links pre-fill once each", { var c = SharedContent(); c.texts = [" a ", "https://x.example", "a"]; return c.prefill }() == "a\nhttps://x.example")
check("a hostile file name is made safe", ShareFiles.sanitize("../../etc/passwd") == "_.._etc_passwd" && ShareFiles.sanitize("...") == "file")

// The files go once the core has stored them.
var withFile = SharedContent()
let photo = tempFile("p.jpg", bytes: 10)
withFile.add(photo)
m = unlockedModel(withFile)
m.toggle(m.targets[1])
m.send()
check("files are deleted once stored", !FileManager.default.fileExists(atPath: photo.url.path))

// Cancel deletes them too, and closes.
var cancelled = SharedContent()
let other = tempFile("q.jpg", bytes: 10)
cancelled.add(other)
m = unlockedModel(cancelled)
finishedWith = nil
m.onFinish = { finishedWith = $0 }
m.cancel()
check("cancel deletes the files and closes", !FileManager.default.fileExists(atPath: other.url.path) && stub_close_count() == 1 && finishedWith == true)

// MARK: Intake

var intakeDone = false
var intake = SharedContent()
let item = NSExtensionItem()
let textFile = dir.appendingPathComponent("notes.txt")
try? "hi".write(to: textFile, atomically: true, encoding: .utf8)
item.attachments = [
  NSItemProvider(item: "shared words" as NSString, typeIdentifier: UTType.plainText.identifier),
  NSItemProvider(item: URL(string: "https://example.com/a")! as NSURL, typeIdentifier: UTType.url.identifier),
  NSItemProvider(contentsOf: textFile)!,
]
Task {
  intake = await ShareIntake.load([item])
  intakeDone = true
}
// Providers may answer on the main queue: keep it turning while the intake runs.
let deadline = Date().addingTimeInterval(20)
while !intakeDone && Date() < deadline {
  RunLoop.main.run(until: Date().addingTimeInterval(0.05))
}
check("intake finished", intakeDone)
check("text and a link become the message", intake.prefill == "shared words\nhttps://example.com/a", intake.prefill)
check("a file is copied into the work directory", intake.files.count == 1 && intake.files[0].url.path.contains("pollis-share") && intake.files[0].size == 2)
intake.deleteFiles()

try? FileManager.default.removeItem(at: dir)
print(failures == 0 ? "all passed" : "\(failures) failed")
exit(failures == 0 ? 0 : 1)
