// What was shared, and the rules for what Pollis will send (#2100). Files are only ever paths in
// the extension's own temporary directory (never the App Group, never Data in memory): copied
// from the sharing app's provider, converted file to file where a phone or desktop elsewhere could
// not show the original (HEIC photos, HEVC video, as the app's picker does with its Compatible
// representation, lib/pickerOptions.ts), and deleted once the core has stored them or the share
// is cancelled. Foundation, ImageIO and AVFoundation only: the macOS harness compiles this file.

import AVFoundation
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// The existing attachment cap (`R2_MEDIA_MAX_BYTES`).
let shareMaxFileBytes: Int64 = 100 * 1024 * 1024
/// Most files one share carries: the activation rule and the core stop here too.
let shareMaxItems = 10
/// Most chats one share goes to.
let shareMaxTargets = 5

enum ShareItemKind: Equatable {
  case photo
  case video
  case file
}

struct ShareFile: Equatable, Identifiable {
  let url: URL
  let filename: String
  let contentType: String
  let kind: ShareItemKind
  let size: Int64
  var width: Int?
  var height: Int?
  /// A video's poster: a 640-edge JPEG, base64.
  var poster: String?

  var id: String { url.path }

  var outgoing: OutgoingFile {
    OutgoingFile(path: url.path, filename: filename, contentType: contentType, width: width, height: height, poster: poster)
  }
}

/// What intake produced.
struct SharedContent: Equatable {
  var files: [ShareFile] = []
  /// Shared text and links, in order; they pre-fill the message.
  var texts: [String] = []
  /// Files refused for size, by name.
  var tooLarge: [String] = []
  /// More files were offered than a share carries.
  var tooMany = false
  /// Something could not be read at all.
  var unreadable = 0

  /// The message box's starting text: every shared text and link, one per line.
  var prefill: String {
    var seen = Set<String>()
    return texts
      .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
      .filter { !$0.isEmpty && seen.insert($0).inserted }
      .joined(separator: "\n")
  }

  /// Nothing Pollis can send: no file and no text.
  var isEmpty: Bool { files.isEmpty && prefill.isEmpty }

  /// The refusal to show, if any (the first that applies).
  var issue: String? {
    if let name = tooLarge.first {
      return ShareStrings.t("tooLarge", name)
    }
    if tooMany {
      return ShareStrings.t("tooMany")
    }
    if isEmpty {
      return ShareStrings.t("nothingToSend")
    }
    return nil
  }

  /// Add a file, holding the size and count limits. Refused files are deleted at once.
  mutating func add(_ file: ShareFile) {
    if file.size > shareMaxFileBytes {
      tooLarge.append(file.filename)
      try? FileManager.default.removeItem(at: file.url)
      return
    }
    if files.count >= shareMaxItems {
      tooMany = true
      try? FileManager.default.removeItem(at: file.url)
      return
    }
    files.append(file)
  }

  /// Delete every temporary file. Called once the core has stored them, and on cancel.
  func deleteFiles() {
    for f in files {
      try? FileManager.default.removeItem(at: f.url)
    }
  }
}

enum ShareFiles {
  /// The extension's own temporary directory for this share; never the App Group.
  static func workDirectory() -> URL {
    let dir = FileManager.default.temporaryDirectory.appendingPathComponent("pollis-share", isDirectory: true)
    try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir
  }

  /// Remove what an earlier share left (a killed extension).
  static func sweep() {
    try? FileManager.default.removeItem(at: workDirectory())
    _ = workDirectory()
  }

  /// A unique path in the work directory keeping `name` (sanitized) for the recipient.
  static func destination(for name: String) -> URL {
    let safe = sanitize(name)
    let dir = workDirectory().appendingPathComponent(UUID().uuidString, isDirectory: true)
    try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir.appendingPathComponent(safe)
  }

  /// A file name with no path separators or control characters, never empty or hidden.
  static func sanitize(_ name: String) -> String {
    let cleaned = name.unicodeScalars.map { scalar -> Character in
      if scalar == "/" || scalar == "\\" || scalar == ":" || CharacterSet.controlCharacters.contains(scalar) {
        return "_"
      }
      return Character(scalar)
    }
    var out = String(cleaned).trimmingCharacters(in: .whitespacesAndNewlines)
    while out.hasPrefix(".") {
      out.removeFirst()
    }
    if out.isEmpty {
      out = "file"
    }
    return String(out.prefix(120))
  }

  static func size(of url: URL) -> Int64 {
    ((try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? NSNumber)?.int64Value ?? 0
  }

  static func mimeType(for url: URL) -> String {
    UTType(filenameExtension: url.pathExtension)?.preferredMIMEType ?? "application/octet-stream"
  }

  /// A file name with `ext` in place of its extension.
  static func renamed(_ name: String, ext: String) -> String {
    let base = (name as NSString).deletingPathExtension
    return "\(base.isEmpty ? "file" : base).\(ext)"
  }

  // MARK: Photos

  /// The pixel size ImageIO reads from the file's properties, orientation applied.
  static func imageSize(_ url: URL) -> (Int, Int)? {
    guard let src = CGImageSourceCreateWithURL(url as CFURL, nil),
          let props = CGImageSourceCopyPropertiesAtIndex(src, 0, nil) as? [CFString: Any],
          let w = props[kCGImagePropertyPixelWidth] as? Int,
          let h = props[kCGImagePropertyPixelHeight] as? Int
    else {
      return nil
    }
    let orientation = props[kCGImagePropertyOrientation] as? Int ?? 1
    return orientation >= 5 ? (h, w) : (w, h)
  }

  /// Whether a photo must become JPEG before it is sent (HEIC/HEIF, which desktop webviews on
  /// Linux and Windows cannot decode).
  static func needsJPEG(_ type: UTType) -> Bool {
    type.conforms(to: .heic) || type.conforms(to: .heif)
  }

  /// Re-encode `source` as JPEG at `dest`, file to file, keeping its metadata (orientation).
  static func convertToJPEG(_ source: URL, to dest: URL) -> Bool {
    guard let src = CGImageSourceCreateWithURL(source as CFURL, nil),
          let out = CGImageDestinationCreateWithURL(dest as CFURL, UTType.jpeg.identifier as CFString, 1, nil)
    else {
      return false
    }
    let options: [CFString: Any] = [kCGImageDestinationLossyCompressionQuality: 0.9]
    CGImageDestinationAddImageFromSource(out, src, 0, options as CFDictionary)
    return CGImageDestinationFinalize(out)
  }

  // MARK: Video

  /// Whether the first video track is HEVC.
  static func isHEVC(_ url: URL) async -> Bool {
    let asset = AVURLAsset(url: url)
    guard let track = try? await asset.loadTracks(withMediaType: .video).first,
          let formats = try? await track.load(.formatDescriptions)
    else {
      return false
    }
    return formats.contains { CMFormatDescriptionGetMediaSubType($0) == kCMVideoCodecType_HEVC }
  }

  /// Export `source` as H.264 MP4 at `dest`. Streams through AVFoundation; nothing is held whole.
  static func exportH264(_ source: URL, to dest: URL) async -> Bool {
    let asset = AVURLAsset(url: source)
    guard let session = AVAssetExportSession(asset: asset, presetName: AVAssetExportPresetHighestQuality) else {
      return false
    }
    session.outputURL = dest
    session.outputFileType = .mp4
    session.shouldOptimizeForNetworkUse = true
    await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
      session.exportAsynchronously {
        done.resume()
      }
    }
    return session.status == .completed
  }

  /// The displayed size of a video, and its poster: a frame half a second in (or a tenth of a
  /// shorter clip), at most 640 on its longest edge, as base64 JPEG. The app's poster rule (#1837).
  static func videoPoster(_ url: URL) async -> (width: Int?, height: Int?, poster: String?) {
    let asset = AVURLAsset(url: url)
    var width: Int?
    var height: Int?
    if let track = try? await asset.loadTracks(withMediaType: .video).first,
       let natural = try? await track.load(.naturalSize),
       let transform = try? await track.load(.preferredTransform)
    {
      let shown = natural.applying(transform)
      width = Int(abs(shown.width).rounded())
      height = Int(abs(shown.height).rounded())
    }
    let duration = (try? await asset.load(.duration)).map(CMTimeGetSeconds) ?? 0
    let at = duration > 0 ? min(0.5, duration / 10) : 0
    let generator = AVAssetImageGenerator(asset: asset)
    generator.appliesPreferredTrackTransform = true
    generator.maximumSize = CGSize(width: 640, height: 640)
    guard let image = try? generator.copyCGImage(at: CMTime(seconds: at, preferredTimescale: 600), actualTime: nil) else {
      return (width, height, nil)
    }
    let data = NSMutableData()
    guard let dest = CGImageDestinationCreateWithData(data, UTType.jpeg.identifier as CFString, 1, nil) else {
      return (width, height, nil)
    }
    CGImageDestinationAddImage(dest, image, [kCGImageDestinationLossyCompressionQuality: 0.8] as CFDictionary)
    guard CGImageDestinationFinalize(dest) else {
      return (width, height, nil)
    }
    return (width, height, (data as Data).base64EncodedString())
  }

  /// A file already copied into the work directory, measured and, where needed, converted.
  static func prepare(_ url: URL, filename: String, type: UTType) async -> ShareFile {
    if type.conforms(to: .image) {
      var file = url
      var name = filename
      if needsJPEG(type) {
        let jpeg = destination(for: renamed(filename, ext: "jpg"))
        if convertToJPEG(url, to: jpeg) {
          try? FileManager.default.removeItem(at: url)
          file = jpeg
          name = jpeg.lastPathComponent
        }
      }
      let size = imageSize(file)
      return ShareFile(
        url: file, filename: name, contentType: mimeType(for: file), kind: .photo,
        size: self.size(of: file), width: size?.0, height: size?.1, poster: nil
      )
    }
    if type.conforms(to: .movie) {
      var file = url
      var name = filename
      if await isHEVC(url) {
        let mp4 = destination(for: renamed(filename, ext: "mp4"))
        if await exportH264(url, to: mp4) {
          try? FileManager.default.removeItem(at: url)
          file = mp4
          name = mp4.lastPathComponent
        }
      }
      let poster = await videoPoster(file)
      return ShareFile(
        url: file, filename: name, contentType: mimeType(for: file), kind: .video,
        size: self.size(of: file), width: poster.width, height: poster.height, poster: poster.poster
      )
    }
    return ShareFile(
      url: url, filename: filename, contentType: mimeType(for: url), kind: .file,
      size: size(of: url), width: nil, height: nil, poster: nil
    )
  }
}
