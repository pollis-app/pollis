// Rewrites a content-free push into "sender: preview" by decrypting on the device (#1599).
//
// The push carries only the opaque handle `h` (aps.mutable-content = 1, data under `body`).
// pollis_notify_fetch opens the shared data dir, fetches and decrypts pending envelopes, stores
// them as the app would, and returns previews. Any failure delivers the push as it arrived.

import Intents
import os
import UserNotifications

// Outcomes only (mode, reason, counts) — never a name, id, handle or body.
private let log = Logger(subsystem: "com.pollis.ios.NotificationService", category: "nse")

final class NotificationService: UNNotificationServiceExtension {
  // Leaves headroom under the ~30 s the system grants before serviceExtensionTimeWillExpire.
  private static let budgetMs: UInt32 = 20_000

  private let lock = NSLock()
  private var contentHandler: ((UNNotificationContent) -> Void)?
  private var original: UNNotificationContent?

  override func didReceive(
    _ request: UNNotificationRequest,
    withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void
  ) {
    lock.lock()
    self.contentHandler = contentHandler
    self.original = request.content
    lock.unlock()

    guard let dataDir = PollisShared.dataDirectory() else {
      log.error("no app group container; delivering the push as it arrived")
      deliver(request.content)
      return
    }
    let handle = Self.handle(of: request.content.userInfo)
    DispatchQueue.global(qos: .userInitiated).async {
      let started = Date()
      let result = PollisNotifyResult.fetch(dataDir: dataDir.path, handle: handle, budgetMs: Self.budgetMs)
      let ms = Int(Date().timeIntervalSince(started) * 1000)
      if let result, result.ok {
        log.info("fetch ok mode=\(result.mode ?? "?", privacy: .public) items=\(result.items?.count ?? 0, privacy: .public) ms=\(ms, privacy: .public)")
      } else {
        log.info("fetch failed reason=\(result?.reason ?? "unparsable", privacy: .public) ms=\(ms, privacy: .public)")
      }
      Self.rewrite(request.content, with: result) { content in
        self.deliver(content)
      }
    }
  }

  override func serviceExtensionTimeWillExpire() {
    lock.lock()
    let fallback = original
    lock.unlock()
    if let fallback {
      log.error("time expired; delivering the push as it arrived")
      deliver(fallback)
    }
  }

  // Exactly once: whichever of the fetch and the expiry timer gets here first wins.
  private func deliver(_ content: UNNotificationContent) {
    lock.lock()
    let handler = contentHandler
    contentHandler = nil
    lock.unlock()
    handler?(content)
  }

  // expo-notifications puts the push's `data` under `body`; `h` is the only field in it.
  static func handle(of userInfo: [AnyHashable: Any]) -> String? {
    guard let body = userInfo["body"] as? [String: Any], let h = body["h"] as? String, !h.isEmpty else {
      return nil
    }
    return h
  }

  static func rewrite(
    _ original: UNNotificationContent,
    with result: PollisNotifyResult?,
    completion: @escaping (UNNotificationContent) -> Void
  ) {
    guard let result, result.ok else {
      completion(original)
      return
    }
    let mode = result.mode ?? "full"
    let visible = (result.items ?? []).filter { !$0.muted }
    // "Nothing", every pending message muted, or nothing new: the generic text, localised.
    // iOS will not let an extension drop a push without the filtering entitlement, so a muted
    // conversation still shows the generic text — exactly what it showed before #1599.
    guard mode == "full" || mode == "name", let item = visible.last else {
      completion(generic(from: original))
      return
    }

    let content = (original.mutableCopy() as? UNMutableNotificationContent) ?? UNMutableNotificationContent()
    let isGroup = !item.isDm
    let conversation = item.conversationTitle?.isEmpty == false ? item.conversationTitle : nil
    if isGroup, let conversation {
      content.title = String(format: L10n.groupTitle, item.senderName, conversation)
    } else {
      content.title = item.senderName
    }
    content.body = mode == "full" ? preview(of: item) : L10n.nameOnlyBody
    content.threadIdentifier = item.conversationId

    communicationContent(content, item: item, groupName: isGroup ? conversation : nil, completion: completion)
  }

  static func preview(of item: PollisNotifyItem) -> String {
    switch item.bodyKey {
    case "attachment": return L10n.attachment
    case "image": return L10n.image
    case "video": return L10n.video
    case "voice": return L10n.voice
    case "sensitive": return L10n.sensitive
    default: return item.body.isEmpty ? L10n.genericBody : item.body
    }
  }

  static func generic(from original: UNNotificationContent) -> UNNotificationContent {
    let content = (original.mutableCopy() as? UNMutableNotificationContent) ?? UNMutableNotificationContent()
    content.title = L10n.genericTitle
    content.body = L10n.genericBody
    return content
  }

  // The Messages-style look (sender monogram, group name). Needs the Communication Notifications
  // capability; without it `updating(from:)` throws and the plain rewrite is delivered instead.
  // The intent carries no message text, so the system's interaction store never holds any.
  static func communicationContent(
    _ content: UNMutableNotificationContent,
    item: PollisNotifyItem,
    groupName: String?,
    completion: @escaping (UNNotificationContent) -> Void
  ) {
    let sender = INPerson(
      personHandle: INPersonHandle(value: item.senderName, type: .unknown),
      nameComponents: nil,
      displayName: item.senderName,
      image: nil,
      contactIdentifier: nil,
      customIdentifier: nil
    )
    var recipients: [INPerson]?
    var speakableGroupName: INSpeakableString?
    if let groupName {
      // A group renders as one only with a group name and more than one recipient.
      let me = INPerson(
        personHandle: INPersonHandle(value: "me", type: .unknown),
        nameComponents: nil,
        displayName: nil,
        image: nil,
        contactIdentifier: nil,
        customIdentifier: nil,
        isMe: true
      )
      recipients = [me, sender]
      speakableGroupName = INSpeakableString(spokenPhrase: groupName)
    }
    let intent = INSendMessageIntent(
      recipients: recipients,
      outgoingMessageType: .outgoingMessageText,
      content: nil,
      speakableGroupName: speakableGroupName,
      conversationIdentifier: item.conversationId,
      serviceName: nil,
      sender: sender,
      attachments: nil
    )
    let interaction = INInteraction(intent: intent, response: nil)
    interaction.direction = .incoming
    interaction.donate { _ in
      do {
        completion(try content.updating(from: intent))
      } catch {
        completion(content)
      }
    }
  }
}

enum PollisShared {
  // Written into both Info.plists by plugins/withNotificationServiceExtension.js.
  static func dataDirectory() -> URL? {
    guard let group = Bundle.main.object(forInfoDictionaryKey: "PollisAppGroup") as? String,
          let container = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group)
    else {
      return nil
    }
    return container.appendingPathComponent("pollis", isDirectory: true)
  }
}

struct PollisNotifyItem: Decodable {
  let conversationId: String
  let messageId: String?
  let senderName: String
  let conversationTitle: String?
  let isDm: Bool
  let body: String
  let bodyKey: String?
  let muted: Bool

  enum CodingKeys: String, CodingKey {
    case conversationId = "conversation_id"
    case messageId = "message_id"
    case senderName = "sender_name"
    case conversationTitle = "conversation_title"
    case isDm = "is_dm"
    case body
    case bodyKey = "body_key"
    case muted
  }

  init(from decoder: Decoder) throws {
    let c = try decoder.container(keyedBy: CodingKeys.self)
    conversationId = try c.decode(String.self, forKey: .conversationId)
    messageId = try c.decodeIfPresent(String.self, forKey: .messageId)
    senderName = try c.decode(String.self, forKey: .senderName)
    conversationTitle = try c.decodeIfPresent(String.self, forKey: .conversationTitle)
    isDm = try c.decodeIfPresent(Bool.self, forKey: .isDm) ?? true
    body = try c.decodeIfPresent(String.self, forKey: .body) ?? ""
    bodyKey = try c.decodeIfPresent(String.self, forKey: .bodyKey)
    // Unknown means do not show.
    muted = try c.decodeIfPresent(Bool.self, forKey: .muted) ?? true
  }
}

struct PollisNotifyResult: Decodable {
  let ok: Bool
  let mode: String?
  let items: [PollisNotifyItem]?
  let reason: String?

  static func parse(_ data: Data) -> PollisNotifyResult? {
    try? JSONDecoder().decode(PollisNotifyResult.self, from: data)
  }

  // Blocking; call off the main thread.
  static func fetch(dataDir: String, handle: String?, budgetMs: UInt32) -> PollisNotifyResult? {
    let raw: UnsafeMutablePointer<CChar>? = dataDir.withCString { dir in
      if let handle {
        return handle.withCString { h in pollis_notify_fetch(dir, h, budgetMs) }
      }
      return pollis_notify_fetch(dir, nil, budgetMs)
    }
    guard let raw else {
      return nil
    }
    defer { pollis_string_free(raw) }
    return parse(Data(String(cString: raw).utf8))
  }
}

enum L10n {
  static var genericTitle: String { NSLocalizedString("genericTitle", comment: "") }
  static var genericBody: String { NSLocalizedString("genericBody", comment: "") }
  static var nameOnlyBody: String { NSLocalizedString("nameOnlyBody", comment: "") }
  static var groupTitle: String { NSLocalizedString("groupTitle", comment: "") }
  static var attachment: String { NSLocalizedString("attachment", comment: "") }
  static var image: String { NSLocalizedString("image", comment: "") }
  static var video: String { NSLocalizedString("video", comment: "") }
  static var voice: String { NSLocalizedString("voice", comment: "") }
  static var sensitive: String { NSLocalizedString("sensitive", comment: "") }
}
