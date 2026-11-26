// The share extension's side of the core's C ABI (#2100, pollis-core/include/pollis_share.h):
// typed wrappers and the JSON they exchange. Every call blocks (Argon2id, SQLCipher, network), so
// callers run them off the main thread. Foundation only: the macOS harness compiles this file.

import Foundation
import Security

/// Where the extension stands after `open`.
enum ShareStatus: Equatable {
  case signedOut
  case lockedOut
  case locked(biometric: Bool, attemptsRemaining: Int, userId: String)
  case failed
}

/// What one unlock attempt came to.
enum UnlockResult: Equatable {
  case unlocked
  case incorrect(attemptsRemaining: Int)
  case lockedOut
  case biometricFailed
  case openApp
  case noSession
  case failed
}

struct ShareTarget: Decodable, Equatable, Identifiable {
  let conversationId: String
  let kind: String
  let title: String
  let subtitle: String?
  let avatarUserId: String?
  let groupId: String?

  var id: String { conversationId }
  var isChannel: Bool { kind == "channel" }
  /// The user's own Vault: drawn with a glyph, titled from the catalogue.
  var isVault: Bool { kind == "vault" }

  enum CodingKeys: String, CodingKey {
    case conversationId = "conversation_id"
    case kind
    case title
    case subtitle
    case avatarUserId = "avatar_user_id"
    case groupId = "group_id"
  }
}

enum TargetsResult: Equatable {
  case ok([ShareTarget])
  case failed(reason: String)
}

/// One target's outcome: `sent`, `queued` (the app finishes it) or `rejected`.
struct TargetOutcome: Decodable, Equatable {
  let conversationId: String
  let messageId: String
  let status: String

  enum CodingKeys: String, CodingKey {
    case conversationId = "conversation_id"
    case messageId = "message_id"
    case status
  }
}

/// A progress event from `pollis_share_send`.
enum SendEvent: Equatable {
  case stored
  case target(TargetOutcome)
}

enum SendResult: Equatable {
  case ok([TargetOutcome])
  case failed(reason: String)
}

/// One file handed to the core: by path, never as bytes.
struct OutgoingFile: Encodable, Equatable {
  let path: String
  let filename: String
  let contentType: String
  let width: Int?
  let height: Int?
  let poster: String?
}

struct ShareRequest: Encodable {
  let shareId: String
  let targets: [String]
  let text: String
  let attachments: [OutgoingFile]
}

/// The core, behind a protocol so the model can be driven by a fake in tests.
protocol ShareCoreAPI: AnyObject {
  func open(dataDir: String) -> ShareStatus
  func unlock(pin: String) -> UnlockResult
  func unlock(deviceKey: String) -> UnlockResult
  func targets(query: String, limit: Int) -> TargetsResult
  func send(_ request: ShareRequest, progress: @escaping (SendEvent) -> Void) -> SendResult
  func suspend(timeoutMs: UInt32) -> Bool
  func resume()
  func close()
}

/// The real core, through the C ABI.
final class PollisShareCore: ShareCoreAPI {
  static func json(_ raw: UnsafeMutablePointer<CChar>?) -> [String: Any] {
    guard let raw else {
      return [:]
    }
    defer { pollis_string_free(raw) }
    let data = Data(String(cString: raw).utf8)
    return (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
  }

  static func status(from v: [String: Any]) -> ShareStatus {
    guard v["ok"] as? Bool == true else {
      return .failed
    }
    switch v["state"] as? String {
    case "signed_out":
      return .signedOut
    case "locked_out":
      return .lockedOut
    case "locked":
      return .locked(
        biometric: v["biometric"] as? Bool ?? false,
        attemptsRemaining: v["attempts_remaining"] as? Int ?? 0,
        userId: v["user_id"] as? String ?? ""
      )
    default:
      return .failed
    }
  }

  static func unlockResult(from v: [String: Any]) -> UnlockResult {
    guard v["ok"] as? Bool == true else {
      return .failed
    }
    switch v["result"] as? String {
    case "unlocked":
      return .unlocked
    case "incorrect":
      return .incorrect(attemptsRemaining: v["attempts_remaining"] as? Int ?? 0)
    case "locked_out":
      return .lockedOut
    case "biometric_failed":
      return .biometricFailed
    case "open_app":
      return .openApp
    case "no_session":
      return .noSession
    default:
      return .failed
    }
  }

  static func targetsResult(from raw: Data) -> TargetsResult {
    struct Wire: Decodable {
      let ok: Bool
      let targets: [ShareTarget]?
      let reason: String?
    }
    guard let wire = try? JSONDecoder().decode(Wire.self, from: raw) else {
      return .failed(reason: "internal")
    }
    if wire.ok, let targets = wire.targets {
      return .ok(targets)
    }
    return .failed(reason: wire.reason ?? "internal")
  }

  static func sendResult(from raw: Data) -> SendResult {
    struct Wire: Decodable {
      let ok: Bool
      let results: [TargetOutcome]?
      let reason: String?
    }
    guard let wire = try? JSONDecoder().decode(Wire.self, from: raw) else {
      return .failed(reason: "internal")
    }
    if wire.ok, let results = wire.results {
      return .ok(results)
    }
    return .failed(reason: wire.reason ?? "internal")
  }

  static func event(from raw: Data) -> SendEvent? {
    guard let v = (try? JSONSerialization.jsonObject(with: raw)) as? [String: Any] else {
      return nil
    }
    switch v["event"] as? String {
    case "stored":
      return .stored
    case "target":
      return (try? JSONDecoder().decode(TargetOutcome.self, from: raw)).map(SendEvent.target)
    default:
      return nil
    }
  }

  private static func string(_ raw: UnsafeMutablePointer<CChar>?) -> Data {
    guard let raw else {
      return Data()
    }
    defer { pollis_string_free(raw) }
    return Data(String(cString: raw).utf8)
  }

  func open(dataDir: String) -> ShareStatus {
    if let group = Bundle.main.object(forInfoDictionaryKey: "PollisKeychainAccessGroup") as? String {
      group.withCString { pollis_set_keychain_access_group($0) }
    }
    return Self.status(from: Self.json(dataDir.withCString { pollis_share_open($0) }))
  }

  func unlock(pin: String) -> UnlockResult {
    Self.unlockResult(from: Self.json(pin.withCString { pollis_share_unlock_pin($0) }))
  }

  func unlock(deviceKey: String) -> UnlockResult {
    Self.unlockResult(from: Self.json(deviceKey.withCString { pollis_share_unlock_device_key($0) }))
  }

  func targets(query: String, limit: Int) -> TargetsResult {
    let raw = query.withCString { pollis_share_targets($0, UInt32(max(0, limit))) }
    return Self.targetsResult(from: Self.string(raw))
  }

  /// Carries the Swift progress closure through the C callback's context pointer.
  private final class ProgressBox {
    let handler: (SendEvent) -> Void
    init(_ handler: @escaping (SendEvent) -> Void) {
      self.handler = handler
    }
  }

  func send(_ request: ShareRequest, progress: @escaping (SendEvent) -> Void) -> SendResult {
    guard let body = try? JSONEncoder().encode(request), let json = String(data: body, encoding: .utf8) else {
      return .failed(reason: "invalid")
    }
    let box = Unmanaged.passRetained(ProgressBox(progress))
    defer { box.release() }
    let callback: pollis_share_progress_fn = { ctx, event in
      guard let ctx, let event else {
        return
      }
      let box = Unmanaged<ProgressBox>.fromOpaque(ctx).takeUnretainedValue()
      if let parsed = PollisShareCore.event(from: Data(String(cString: event).utf8)) {
        box.handler(parsed)
      }
    }
    let raw = json.withCString { pollis_share_send($0, callback, box.toOpaque()) }
    return Self.sendResult(from: Self.string(raw))
  }

  func suspend(timeoutMs: UInt32) -> Bool {
    pollis_share_suspend(timeoutMs)
  }

  func resume() {
    pollis_share_resume()
  }

  func close() {
    pollis_share_close()
  }
}

/// Lexicographically sortable unique ids, as the core's `ulid` crate makes them: 48 bits of
/// milliseconds and 80 random bits, Crockford base32. A share mints one and reuses it on retry, so
/// the core names the same rows every time.
enum ULID {
  private static let alphabet = Array("0123456789ABCDEFGHJKMNPQRSTVWXYZ")

  static func make(date: Date = Date()) -> String {
    let ms = UInt64(max(0, date.timeIntervalSince1970 * 1000))
    var bytes = [UInt8](repeating: 0, count: 16)
    for i in 0..<6 {
      bytes[i] = UInt8((ms >> (8 * UInt64(5 - i))) & 0xFF)
    }
    var random = [UInt8](repeating: 0, count: 10)
    if SecRandomCopyBytes(kSecRandomDefault, random.count, &random) != errSecSuccess {
      for i in random.indices {
        random[i] = UInt8.random(in: 0...255)
      }
    }
    bytes.replaceSubrange(6..<16, with: random)
    return encode(bytes)
  }

  /// 128 bits as 26 base32 characters (the first carries 3 bits).
  static func encode(_ bytes: [UInt8]) -> String {
    var value = (UInt64(0), UInt64(0))
    for (i, b) in bytes.enumerated() {
      if i < 8 {
        value.0 = (value.0 << 8) | UInt64(b)
      } else {
        value.1 = (value.1 << 8) | UInt64(b)
      }
    }
    var out = [Character]()
    for index in 0..<26 {
      // Bit offset of this character's 5 bits from the top of the 130-bit field (2 leading zeros).
      let shift = 125 - index * 5
      var chunk: UInt64 = 0
      for bit in 0..<5 {
        let pos = shift + 4 - bit
        let set: Bool
        if pos >= 128 {
          set = false
        } else if pos >= 64 {
          set = (value.0 >> UInt64(pos - 64)) & 1 == 1
        } else {
          set = (value.1 >> UInt64(pos)) & 1 == 1
        }
        chunk = (chunk << 1) | (set ? 1 : 0)
      }
      out.append(alphabet[Int(chunk)])
    }
    return String(out)
  }
}
