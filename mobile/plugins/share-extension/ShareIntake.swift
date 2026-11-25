// Reads what the sharing app handed over (#2100): `NSExtensionItem` attachments, one provider at a
// time and in order. Text and web links pre-fill the message; files are copied by the file system
// into the extension's own temporary directory inside the provider's callback (its copy is gone
// after it), then measured and converted (ShareItems.swift). Nothing is loaded as Data.

import Foundation
import UniformTypeIdentifiers

enum ShareIntake {
  static func load(_ items: [NSExtensionItem]) async -> SharedContent {
    var content = SharedContent()
    for item in items {
      for provider in item.attachments ?? [] {
        await take(provider, into: &content)
      }
    }
    return content
  }

  /// The type to ask a provider for, by what the share is: a JPEG photo when one is offered.
  static func fileType(of provider: NSItemProvider) -> UTType? {
    let registered = provider.registeredTypeIdentifiers.compactMap(UTType.init)
    if registered.contains(where: { $0.conforms(to: .image) }) {
      return registered.first { $0 == .jpeg } ?? registered.first { $0.conforms(to: .image) }
    }
    if let movie = registered.first(where: { $0.conforms(to: .movie) }) {
      return movie
    }
    return registered.first { $0.conforms(to: .data) && !$0.conforms(to: .url) && !$0.conforms(to: .plainText) }
  }

  private static func take(_ provider: NSItemProvider, into content: inout SharedContent) async {
    let has = { (t: UTType) in provider.hasItemConformingToTypeIdentifier(t.identifier) }
    if has(.url), let url = await loadURL(provider) {
      if url.isFileURL {
        if let file = await copyFile(provider, type: .data) {
          content.add(file)
        } else {
          content.unreadable += 1
        }
      } else {
        content.texts.append(url.absoluteString)
      }
      return
    }
    if let type = fileType(of: provider) {
      if content.files.count >= shareMaxItems {
        content.tooMany = true
        return
      }
      if let file = await copyFile(provider, type: type) {
        content.add(file)
      } else {
        content.unreadable += 1
      }
      return
    }
    if has(.plainText), let text = await loadText(provider) {
      content.texts.append(text)
      return
    }
    content.unreadable += 1
  }

  private static func loadURL(_ provider: NSItemProvider) async -> URL? {
    await withCheckedContinuation { done in
      provider.loadItem(forTypeIdentifier: UTType.url.identifier, options: nil) { item, _ in
        if let url = item as? URL {
          done.resume(returning: url)
        } else if let data = item as? Data, let url = URL(dataRepresentation: data, relativeTo: nil) {
          done.resume(returning: url)
        } else if let s = item as? String {
          done.resume(returning: URL(string: s))
        } else {
          done.resume(returning: nil)
        }
      }
    }
  }

  private static func loadText(_ provider: NSItemProvider) async -> String? {
    await withCheckedContinuation { done in
      provider.loadItem(forTypeIdentifier: UTType.plainText.identifier, options: nil) { item, _ in
        if let s = item as? String {
          done.resume(returning: s)
        } else if let s = item as? NSAttributedString {
          done.resume(returning: s.string)
        } else {
          done.resume(returning: nil)
        }
      }
    }
  }

  /// Copy the provider's file into the work directory, then prepare it. Nil when unreadable.
  private static func copyFile(_ provider: NSItemProvider, type: UTType) async -> ShareFile? {
    let suggested = provider.suggestedName
    let copied: (URL, String)? = await withCheckedContinuation { done in
      provider.loadFileRepresentation(forTypeIdentifier: type.identifier) { url, _ in
        guard let url else {
          done.resume(returning: nil)
          return
        }
        var name = suggested ?? url.deletingPathExtension().lastPathComponent
        if (name as NSString).pathExtension.isEmpty, !url.pathExtension.isEmpty {
          name += ".\(url.pathExtension)"
        }
        let dest = ShareFiles.destination(for: name)
        do {
          try FileManager.default.copyItem(at: url, to: dest)
          done.resume(returning: (dest, dest.lastPathComponent))
        } catch {
          done.resume(returning: nil)
        }
      }
    }
    guard let (url, name) = copied else {
      return nil
    }
    let actual = UTType(filenameExtension: url.pathExtension) ?? type
    return await ShareFiles.prepare(url, filename: name, type: actual)
  }
}
