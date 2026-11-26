// The share extension's state machine (#2100): open the core's session, unlock with the app's PIN
// counter or its biometric key, pick up to five chats, send in place. Every core call runs on the
// `work` queue and lands back on `main`; anything the user taps changes state on the same frame
// (busy spinner, disabled Send) so nothing can be submitted twice or look frozen.
// Foundation and Combine only: the macOS harness drives this file against a stub core.

import Combine
import Foundation

final class ShareModel: ObservableObject {
  enum Phase: Equatable {
    /// Reading the shared items, or opening the session.
    case preparing
    /// A screen with a title, a line, and only Cancel: signed out, locked out, update pending,
    /// or nothing Pollis can send.
    case notice(title: String, body: String)
    case locked
    case picking
    /// Every target has an outcome; some are still queued for the app.
    case finished
  }

  @Published private(set) var phase: Phase = .preparing
  @Published private(set) var content = SharedContent()
  @Published var message = ""

  // The keypad.
  @Published private(set) var pin = ""
  @Published private(set) var pinBusy = false
  @Published private(set) var pinError: String?
  @Published private(set) var biometricOffered = false

  // The picker.
  @Published private(set) var query = ""
  @Published private(set) var targets: [ShareTarget] = []
  @Published private(set) var targetsLoading = false
  @Published private(set) var targetsError: String?
  @Published private(set) var selected: [ShareTarget] = []
  @Published private(set) var capReached = false

  // Sending.
  @Published private(set) var sending = false
  @Published private(set) var sendError: String?
  /// conversation id → "sent" | "queued" | "rejected".
  @Published private(set) var outcomes: [String: String] = [:]

  /// Called once, after the session is closed: `true` when the user cancelled.
  var onFinish: ((Bool) -> Void)?

  private let core: ShareCoreAPI
  private let work: (@escaping () -> Void) -> Void
  private let main: (@escaping () -> Void) -> Void
  private let after: (TimeInterval, @escaping () -> Void) -> Void
  private let readBiometric: (String) -> BiometricRead
  private let debounce: TimeInterval

  private var userId = ""
  /// Minted once per share; a retry reuses it, so the core never stores a row twice.
  let shareId = ULID.make()
  private var searchGeneration = 0
  private var finished = false
  private var filesReleased = false

  init(
    core: ShareCoreAPI,
    work: @escaping (@escaping () -> Void) -> Void = { DispatchQueue.global(qos: .userInitiated).async(execute: $0) },
    main: @escaping (@escaping () -> Void) -> Void = { DispatchQueue.main.async(execute: $0) },
    after: @escaping (TimeInterval, @escaping () -> Void) -> Void = { DispatchQueue.main.asyncAfter(deadline: .now() + $0, execute: $1) },
    debounce: TimeInterval = 0.25,
    readBiometric: @escaping (String) -> BiometricRead = { BiometricKey.read(userId: $0, prompt: ShareStrings.t("biometricReason")) }
  ) {
    self.core = core
    self.work = work
    self.main = main
    self.after = after
    self.debounce = debounce
    self.readBiometric = readBiometric
  }

  // MARK: Start

  /// What was shared is read; open the session for whoever is signed in.
  func start(content: SharedContent, dataDir: String?) {
    self.content = content
    message = content.prefill
    if let issue = content.issue {
      phase = .notice(title: issue, body: "")
      return
    }
    guard let dataDir else {
      phase = .notice(title: ShareStrings.t("signedOutTitle"), body: ShareStrings.t("signedOutBody"))
      return
    }
    phase = .preparing
    work { [core] in
      let status = core.open(dataDir: dataDir)
      self.main { self.opened(status) }
    }
  }

  private func opened(_ status: ShareStatus) {
    switch status {
    case .signedOut:
      phase = .notice(title: ShareStrings.t("signedOutTitle"), body: ShareStrings.t("signedOutBody"))
    case .lockedOut:
      phase = .notice(title: ShareStrings.t("lockedOutTitle"), body: ShareStrings.t("lockedOutBody"))
    case .failed:
      phase = .notice(title: ShareStrings.t("openAppTitle"), body: ShareStrings.t("openAppBody"))
    case let .locked(biometric, _, userId):
      self.userId = userId
      biometricOffered = biometric
      phase = .locked
      if biometric {
        tryBiometric()
      }
    }
  }

  // MARK: Unlock

  func typeDigit(_ digit: Character) {
    guard phase == .locked, !pinBusy, pin.count < 4, digit.isASCII, digit.isNumber else {
      return
    }
    pin.append(digit)
    pinError = nil
    if pin.count == 4 {
      let typed = pin
      pinBusy = true
      work { [core] in
        let result = core.unlock(pin: typed)
        self.main { self.unlocked(result) }
      }
    }
  }

  func deleteDigit() {
    guard !pinBusy, !pin.isEmpty else {
      return
    }
    pin.removeLast()
  }

  /// Face ID / Touch ID through the app's key. A cancel or a miss is the keypad, nothing spent.
  func tryBiometric() {
    guard phase == .locked, biometricOffered, !pinBusy else {
      return
    }
    pinBusy = true
    let user = userId
    work { [core, readBiometric] in
      switch readBiometric(user) {
      case let .key(key):
        let result = core.unlock(deviceKey: key)
        self.main { self.unlocked(result) }
      case .cancelled:
        self.main { self.pinBusy = false }
      case .missing:
        self.main {
          self.pinBusy = false
          self.pinError = ShareStrings.t("biometricChanged")
        }
      }
    }
  }

  private func unlocked(_ result: UnlockResult) {
    pinBusy = false
    pin = ""
    switch result {
    case .unlocked:
      pinError = nil
      phase = .picking
      loadTargets()
    case let .incorrect(left):
      pinError = ShareStrings.plural("pinIncorrect", left)
    case .lockedOut:
      phase = .notice(title: ShareStrings.t("lockedOutTitle"), body: ShareStrings.t("lockedOutBody"))
    case .biometricFailed:
      pinError = ShareStrings.t("biometricChanged")
    case .openApp, .noSession, .failed:
      phase = .notice(title: ShareStrings.t("openAppTitle"), body: ShareStrings.t("openAppBody"))
    }
  }

  // MARK: Picker

  func search(_ text: String) {
    query = text
    searchGeneration += 1
    let generation = searchGeneration
    after(debounce) {
      if generation == self.searchGeneration {
        self.loadTargets()
      }
    }
  }

  func retryTargets() {
    loadTargets()
  }

  private func loadTargets() {
    searchGeneration += 1
    let generation = searchGeneration
    let q = query
    targetsLoading = true
    targetsError = nil
    work { [core] in
      let result = core.targets(query: q, limit: 0)
      self.main {
        guard generation == self.searchGeneration else {
          return
        }
        self.targetsLoading = false
        switch result {
        case let .ok(rows):
          self.targets = rows
        case .failed:
          self.targetsError = ShareStrings.t("loadFailed")
        }
      }
    }
  }

  func isSelected(_ target: ShareTarget) -> Bool {
    selected.contains { $0.id == target.id }
  }

  func toggle(_ target: ShareTarget) {
    guard !sending else {
      return
    }
    if let i = selected.firstIndex(where: { $0.id == target.id }) {
      selected.remove(at: i)
      capReached = false
      return
    }
    if selected.count >= shareMaxTargets {
      capReached = true
      return
    }
    selected.append(target)
  }

  // MARK: Send

  var canSend: Bool {
    !sending && !selected.isEmpty
      && (!content.files.isEmpty || !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
  }

  func send() {
    guard canSend, phase == .picking else {
      return
    }
    // Busy on this frame: a second tap finds `sending` set.
    sending = true
    sendError = nil
    outcomes = [:]
    let request = ShareRequest(
      shareId: shareId,
      targets: selected.map(\.conversationId),
      text: message.trimmingCharacters(in: .whitespacesAndNewlines),
      attachments: content.files.map(\.outgoing)
    )
    work { [core] in
      let result = core.send(request) { event in
        self.main { self.progress(event) }
      }
      self.main { self.sent(result) }
    }
  }

  private func progress(_ event: SendEvent) {
    switch event {
    case .stored:
      // Every target's copy is in the core's encrypted database: the plaintext copies can go.
      releaseFiles()
    case let .target(outcome):
      outcomes[outcome.conversationId] = outcome.status
    }
  }

  private func sent(_ result: SendResult) {
    switch result {
    case let .ok(results):
      for r in results {
        outcomes[r.conversationId] = r.status
      }
      sending = false
      if results.allSatisfy({ $0.status == "sent" }) {
        finish(cancelled: false)
      } else {
        phase = .finished
      }
    case .failed:
      sending = false
      sendError = ShareStrings.t("sendFailed")
    }
  }

  /// Done after some targets were left queued for the app.
  func done() {
    finish(cancelled: false)
  }

  func cancel() {
    guard !sending else {
      return
    }
    finish(cancelled: true)
  }

  private func releaseFiles() {
    if !filesReleased {
      filesReleased = true
      content.deleteFiles()
    }
  }

  /// Close the session (the lock and the database), delete the temporary files, then let the
  /// host complete. Unfinished sends stay in the app's outbox.
  private func finish(cancelled: Bool) {
    guard !finished else {
      return
    }
    finished = true
    releaseFiles()
    work { [core] in
      core.close()
      self.main { self.onFinish?(cancelled) }
    }
  }

  // MARK: Host lifecycle

  /// The host app left the foreground: let sends finish or park, then hold nothing.
  /// Blocking (up to 20 s): call from the expiring activity's thread. True when nothing is held.
  @discardableResult
  func hostWillResignActive() -> Bool {
    core.suspend(timeoutMs: 20_000)
  }

  /// The background time ran out: suspend at once, without waiting for sends.
  func hostTimeExpired() -> Bool {
    core.suspend(timeoutMs: 0)
  }

  func hostDidBecomeActive() {
    core.resume()
  }
}
