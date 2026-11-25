// The share extension's entry point (#2100): reads what was shared, hosts the SwiftUI sheet, and
// keeps the core's session from being suspended while it holds the App Group database or
// `state.lock` (`0xdead10cc`): when the host app leaves the foreground the session parks inside an
// expiring activity, and it resumes when the host comes back. Logs outcomes only, never content.

import os
import SwiftUI
import UIKit

private let log = Logger(subsystem: "com.pollis.ios.Share", category: "share")

@objc(ShareViewController)
final class ShareViewController: UIViewController {
  private let model = ShareModel(core: PollisShareCore())
  private var observers: [NSObjectProtocol] = []

  override func viewDidLoad() {
    super.viewDidLoad()
    ShareStrings.bundle = Bundle(for: ShareViewController.self)
    let theme = ShareTheme.current()
    view.backgroundColor = UIColor(theme.bg)
    let host = UIHostingController(rootView: ShareRootView(model: model, theme: theme))
    addChild(host)
    host.view.translatesAutoresizingMaskIntoConstraints = false
    host.view.backgroundColor = .clear
    view.addSubview(host.view)
    NSLayoutConstraint.activate([
      host.view.leadingAnchor.constraint(equalTo: view.leadingAnchor),
      host.view.trailingAnchor.constraint(equalTo: view.trailingAnchor),
      host.view.topAnchor.constraint(equalTo: view.topAnchor),
      host.view.bottomAnchor.constraint(equalTo: view.bottomAnchor),
    ])
    host.didMove(toParent: self)

    model.onFinish = { [weak self] cancelled in
      guard let self else {
        return
      }
      log.info("finished cancelled=\(cancelled, privacy: .public)")
      if cancelled {
        self.extensionContext?.cancelRequest(withError: NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError))
      } else {
        self.extensionContext?.completeRequest(returningItems: nil)
      }
    }
    observeHost()

    let items = (extensionContext?.inputItems as? [NSExtensionItem]) ?? []
    let dataDir = Self.dataDirectory()?.path
    Task.detached(priority: .userInitiated) { [model] in
      ShareFiles.sweep()
      let content = await ShareIntake.load(items)
      log.info("intake files=\(content.files.count, privacy: .public) texts=\(content.texts.count, privacy: .public) refused=\(content.tooLarge.count, privacy: .public)")
      await MainActor.run {
        model.start(content: content, dataDir: dataDir)
      }
    }
  }

  deinit {
    observers.forEach(NotificationCenter.default.removeObserver)
  }

  /// The host app's foreground state, through the extension notifications iOS posts.
  private func observeHost() {
    let center = NotificationCenter.default
    observers.append(center.addObserver(forName: .NSExtensionHostWillResignActive, object: nil, queue: .main) { [model] _ in
      ProcessInfo.processInfo.performExpiringActivity(withReason: "Pollis share") { expiring in
        guard !expiring else {
          return
        }
        let released = model.hostWillResignActive()
        log.info("suspended released=\(released, privacy: .public)")
      }
    })
    observers.append(center.addObserver(forName: .NSExtensionHostDidBecomeActive, object: nil, queue: .main) { [model] _ in
      DispatchQueue.global(qos: .userInitiated).async {
        model.hostDidBecomeActive()
      }
    })
  }

  static func dataDirectory() -> URL? {
    guard let group = Bundle.main.object(forInfoDictionaryKey: "PollisAppGroup") as? String,
          let container = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group)
    else {
      return nil
    }
    return container.appendingPathComponent("pollis", isDirectory: true)
  }
}
