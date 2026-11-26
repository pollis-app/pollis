// The share extension's screens (#2100), in SwiftUI: the keypad, the chat picker and the notices.
// Colours come from the theme the app last saved (`pollis.theme.*` in the App Group, the
// theme/derive.ts recipe through LetterAvatar), so the sheet looks like Pollis. Controls are filled,
// never bordered; the primary action is the accent with a dark label; selected chats are filled
// pills. Accessibility identifiers (`share-*`) are what UI automation finds.

import ImageIO
import SwiftUI
import UIKit
import UniformTypeIdentifiers

struct ShareTheme {
  let bg: Color
  let surface: Color
  let raised: Color
  let fg: Color
  let dim: Color
  let accent: Color
  let onAccent: Color
  let danger: Color
  let raw: (accent: UInt32, background: UInt32)

  static func color(_ rgb: UInt32) -> Color {
    Color(
      .sRGB,
      red: Double((rgb >> 16) & 0xFF) / 255,
      green: Double((rgb >> 8) & 0xFF) / 255,
      blue: Double(rgb & 0xFF) / 255,
      opacity: 1
    )
  }

  static func luminance(_ rgb: UInt32) -> Double {
    let c = [16, 8, 0].map { Double((rgb >> UInt32($0)) & 0xFF) / 255 }
    return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
  }

  static func current() -> ShareTheme {
    let group = Bundle.main.object(forInfoDictionaryKey: "PollisAppGroup") as? String
    let raw = LetterAvatar.theme(group.flatMap(UserDefaults.init(suiteName:)))
    let light = luminance(raw.background) > 0.5
    let (high, dimLetter) = LetterAvatar.colors(accent: raw.accent, background: raw.background)
    let raised = LetterAvatar.tier(raw.accent, raw.background, accentWeight: 0.05, lighten: 0.04)
    return ShareTheme(
      bg: color(raw.background),
      surface: color(raised),
      raised: color(high),
      fg: light ? Color(white: 0.08) : Color(white: 0.95),
      dim: color(dimLetter),
      accent: color(raw.accent),
      onAccent: luminance(raw.accent) > 0.45 ? Color(white: 0.06) : Color(white: 0.97),
      danger: Color(red: 0.89, green: 0.33, blue: 0.31),
      raw: raw
    )
  }
}

struct ShareRootView: View {
  @ObservedObject var model: ShareModel
  let theme: ShareTheme

  var body: some View {
    VStack(spacing: 0) {
      header
      switch model.phase {
      case .preparing:
        Spacer()
        ProgressView().tint(theme.dim)
        Text(ShareStrings.t("preparing")).foregroundColor(theme.dim).padding(.top, 8)
        Spacer()
      case let .notice(title, body):
        NoticeView(title: title, message: body, theme: theme, close: model.cancel)
      case .locked:
        KeypadView(model: model, theme: theme)
      case .picking:
        PickerView(model: model, theme: theme)
      case .finished:
        FinishedView(model: model, theme: theme)
      }
    }
    .background(theme.bg.ignoresSafeArea())
    .foregroundColor(theme.fg)
  }

  private var header: some View {
    ZStack {
      Text(ShareStrings.t("title")).font(.headline)
      HStack {
        Button(ShareStrings.t("cancel"), action: model.cancel)
          .foregroundColor(theme.fg)
          .disabled(model.sending)
          .accessibilityIdentifier("share-cancel")
        Spacer()
      }
    }
    .padding(.horizontal, 16)
    .padding(.vertical, 14)
  }
}

struct NoticeView: View {
  let title: String
  let message: String
  let theme: ShareTheme
  let close: () -> Void

  var body: some View {
    VStack(spacing: 12) {
      Spacer()
      Text(title)
        .font(.title3.weight(.semibold))
        .multilineTextAlignment(.center)
        .accessibilityIdentifier("share-notice")
      if !message.isEmpty {
        Text(message).foregroundColor(theme.dim).multilineTextAlignment(.center)
      }
      Spacer()
      PrimaryButton(title: ShareStrings.t("done"), busy: false, enabled: true, theme: theme, action: close)
        .accessibilityIdentifier("share-notice-close")
    }
    .padding(24)
  }
}

struct PrimaryButton: View {
  let title: String
  let busy: Bool
  let enabled: Bool
  let theme: ShareTheme
  let action: () -> Void

  var body: some View {
    Button(action: action) {
      HStack(spacing: 8) {
        if busy {
          ProgressView().tint(theme.onAccent)
        }
        Text(title).font(.body.weight(.semibold))
      }
      .frame(maxWidth: .infinity, minHeight: 50)
      .background(enabled || busy ? theme.accent : theme.raised)
      .foregroundColor(enabled || busy ? theme.onAccent : theme.dim)
      .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
    }
    .disabled(!enabled)
  }
}

// MARK: Keypad

struct KeypadView: View {
  @ObservedObject var model: ShareModel
  let theme: ShareTheme

  private let rows: [[String]] = [["1", "2", "3"], ["4", "5", "6"], ["7", "8", "9"], ["bio", "0", "del"]]

  var body: some View {
    VStack(spacing: 18) {
      Spacer(minLength: 8)
      Text(ShareStrings.t("pinHeadline")).font(.title3.weight(.semibold))
      Text(ShareStrings.t("pinSubline")).foregroundColor(theme.dim)
      ZStack {
        if model.pinBusy {
          HStack(spacing: 8) {
            ProgressView().tint(theme.dim)
            Text(ShareStrings.t("unlocking")).foregroundColor(theme.dim)
          }
          .accessibilityIdentifier("share-pin-busy")
        } else {
          HStack(spacing: 16) {
            ForEach(0..<4, id: \.self) { i in
              Circle()
                .fill(i < model.pin.count ? theme.accent : theme.raised)
                .frame(width: 14, height: 14)
            }
          }
        }
      }
      .frame(height: 28)
      Text(model.pinError ?? " ")
        .font(.footnote)
        .foregroundColor(theme.danger)
        .multilineTextAlignment(.center)
        .accessibilityIdentifier("share-pin-error")
      VStack(spacing: 12) {
        ForEach(rows, id: \.self) { row in
          HStack(spacing: 12) {
            ForEach(row, id: \.self) { key in
              keyView(key)
            }
          }
        }
      }
      .padding(.horizontal, 32)
      Spacer(minLength: 8)
    }
    .padding(.bottom, 16)
  }

  @ViewBuilder
  private func keyView(_ key: String) -> some View {
    switch key {
    case "bio":
      if model.biometricOffered {
        Button(action: model.tryBiometric) {
          Image(systemName: "faceid").font(.title2).frame(maxWidth: .infinity, minHeight: 60)
        }
        .foregroundColor(theme.fg)
        .disabled(model.pinBusy)
        .accessibilityLabel(ShareStrings.t("useBiometric"))
        .accessibilityIdentifier("share-pin-biometric")
      } else {
        Color.clear.frame(maxWidth: .infinity, minHeight: 60)
      }
    case "del":
      Button(action: model.deleteDigit) {
        Image(systemName: "delete.left").font(.title2).frame(maxWidth: .infinity, minHeight: 60)
      }
      .foregroundColor(theme.fg)
      .disabled(model.pinBusy)
      .accessibilityIdentifier("share-pin-delete")
    default:
      Button {
        model.typeDigit(Character(key))
      } label: {
        Text(key)
          .font(.title2.weight(.medium))
          .frame(maxWidth: .infinity, minHeight: 60)
          .background(theme.surface)
          .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
      }
      .foregroundColor(theme.fg)
      .disabled(model.pinBusy)
      .accessibilityIdentifier("share-pin-\(key)")
    }
  }
}

// MARK: Picker

struct PickerView: View {
  @ObservedObject var model: ShareModel
  let theme: ShareTheme
  @State private var search = ""
  /// Which field has the keyboard. Return on the message, a scroll, or a tap on a chat puts it
  /// down, so the list and Send are never stuck under it.
  @FocusState private var focus: Field?

  enum Field {
    case message
    case search
  }

  var body: some View {
    VStack(spacing: 10) {
      ScrollView {
        VStack(alignment: .leading, spacing: 12) {
          if !model.content.files.isEmpty {
            PreviewStrip(files: model.content.files, theme: theme)
          }
          TextField(ShareStrings.t("messagePlaceholder"), text: $model.message, axis: .vertical)
            .lineLimit(1...5)
            .focused($focus, equals: .message)
            .submitLabel(.done)
            .onChange(of: model.message) { text in
              // A vertical field takes return as a newline: here it means done.
              if text.hasSuffix("\n") {
                model.message = String(text.dropLast())
                focus = nil
              }
            }
            .padding(12)
            .background(theme.surface)
            .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            .disabled(model.sending)
            .accessibilityIdentifier("share-message")
          if !model.selected.isEmpty {
            selectedPills
          }
          HStack(spacing: 8) {
            Image(systemName: "magnifyingglass").foregroundColor(theme.dim)
            TextField(ShareStrings.t("search"), text: $search)
              .autocorrectionDisabled()
              .textInputAutocapitalization(.never)
              .onChange(of: search) { model.search($0) }
              .focused($focus, equals: .search)
              .submitLabel(.search)
              .onSubmit { focus = nil }
              .accessibilityIdentifier("share-search")
          }
          .padding(12)
          .background(theme.surface)
          .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
          if model.capReached {
            Text(ShareStrings.t("maxTargets")).font(.footnote).foregroundColor(theme.dim)
              .accessibilityIdentifier("share-max-targets")
          }
          targetList
        }
        .padding(.horizontal, 16)
      }
      .scrollDismissesKeyboard(.immediately)
      VStack(spacing: 8) {
        if let error = model.sendError {
          Text(error).font(.footnote).foregroundColor(theme.danger).accessibilityIdentifier("share-send-error")
        }
        PrimaryButton(
          title: model.sending ? ShareStrings.t("sending") : ShareStrings.t("send"),
          busy: model.sending,
          enabled: model.canSend,
          theme: theme,
          action: model.send
        )
        .accessibilityIdentifier("share-send")
      }
      .padding(.horizontal, 16)
      .padding(.bottom, 12)
    }
  }

  private var selectedPills: some View {
    ScrollView(.horizontal, showsIndicators: false) {
      HStack(spacing: 8) {
        ForEach(model.selected) { target in
          Button {
            model.toggle(target)
          } label: {
            HStack(spacing: 6) {
              Text(TargetRow.title(of: target)).lineLimit(1)
              if let status = model.outcomes[target.id] {
                StatusGlyph(status: status, theme: theme, onAccent: true)
              } else {
                Image(systemName: "xmark").font(.caption2.weight(.bold))
              }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 7)
            .background(theme.accent)
            .foregroundColor(theme.onAccent)
            .clipShape(Capsule())
          }
          .disabled(model.sending)
        }
      }
    }
  }

  @ViewBuilder
  private var targetList: some View {
    if model.targetsLoading && model.targets.isEmpty {
      HStack(spacing: 8) {
        ProgressView().tint(theme.dim)
        Text(ShareStrings.t("loading")).foregroundColor(theme.dim)
      }
      .frame(maxWidth: .infinity)
      .padding(.vertical, 24)
    } else if let error = model.targetsError {
      VStack(spacing: 8) {
        Text(error).foregroundColor(theme.dim)
        Button(ShareStrings.t("retry"), action: model.retryTargets)
          .foregroundColor(theme.accent)
          .accessibilityIdentifier("share-targets-retry")
      }
      .frame(maxWidth: .infinity)
      .padding(.vertical, 24)
    } else if model.targets.isEmpty {
      Text(model.query.isEmpty ? ShareStrings.t("noChats") : ShareStrings.t("noMatches"))
        .foregroundColor(theme.dim)
        .frame(maxWidth: .infinity)
        .padding(.vertical, 24)
    } else {
      LazyVStack(spacing: 2) {
        ForEach(Array(model.targets.enumerated()), id: \.element.id) { index, target in
          TargetRow(target: target, selected: model.isSelected(target), theme: theme) {
            focus = nil
            model.toggle(target)
          }
          .disabled(model.sending)
          .accessibilityIdentifier("share-target-\(index)")
        }
      }
    }
  }
}

struct TargetRow: View {
  let target: ShareTarget
  let selected: Bool
  let theme: ShareTheme
  let action: () -> Void

  static func title(of target: ShareTarget) -> String {
    if target.isVault {
      return ShareStrings.t("vault")
    }
    return target.isChannel ? "#\(target.title)" : target.title
  }

  private var subtitle: String? {
    if target.isChannel, let group = target.subtitle {
      return ShareStrings.t("channelIn", group)
    }
    return target.subtitle
  }

  var body: some View {
    Button(action: action) {
      HStack(spacing: 12) {
        avatar
        VStack(alignment: .leading, spacing: 2) {
          Text(Self.title(of: target))
            .fontWeight(selected ? .bold : .regular)
            .foregroundColor(selected ? theme.accent : theme.fg)
            .lineLimit(1)
          if let subtitle {
            Text(subtitle).font(.footnote).foregroundColor(theme.dim).lineLimit(1)
          }
        }
        Spacer()
        // The app's pick-one and pick-many rows: a trailing check on the chosen, nothing on the rest.
        if selected {
          Image(systemName: "checkmark")
            .font(.body.weight(.semibold))
            .foregroundColor(theme.accent)
        }
      }
      .padding(.vertical, 8)
      .padding(.horizontal, 8)
      .background(selected ? theme.surface : Color.clear)
      .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
      .contentShape(Rectangle())
    }
    .foregroundColor(theme.fg)
    .accessibilityAddTraits(selected ? .isSelected : [])
  }

  @ViewBuilder
  private var avatar: some View {
    if target.isVault {
      Image(systemName: "archivebox")
        .font(.body.weight(.semibold))
        .frame(width: 40, height: 40)
        .background(theme.raised)
        .foregroundColor(theme.dim)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
    } else if let png = LetterAvatar.png(label: target.title, rounded: target.isChannel, theme: theme.raw, size: 120),
              let image = UIImage(data: png)
    {
      Image(uiImage: image).resizable().frame(width: 40, height: 40)
    } else {
      Circle().fill(theme.raised).frame(width: 40, height: 40)
    }
  }
}

struct StatusGlyph: View {
  let status: String
  let theme: ShareTheme
  var onAccent = false

  var body: some View {
    switch status {
    case "sent":
      Image(systemName: "checkmark").font(.caption.weight(.bold))
    case "queued":
      Image(systemName: "clock").font(.caption.weight(.bold))
    default:
      Image(systemName: "exclamationmark.circle.fill")
        .font(.caption.weight(.bold))
        .foregroundColor(onAccent ? theme.onAccent : theme.danger)
    }
  }
}

struct PreviewStrip: View {
  let files: [ShareFile]
  let theme: ShareTheme

  var body: some View {
    ScrollView(.horizontal, showsIndicators: false) {
      HStack(spacing: 8) {
        ForEach(files) { file in
          tile(file)
        }
      }
    }
    .accessibilityIdentifier("share-preview")
  }

  @ViewBuilder
  private func tile(_ file: ShareFile) -> some View {
    if let image = Self.thumbnail(file) {
      ZStack {
        Image(uiImage: image).resizable().scaledToFill()
        if file.kind == .video {
          Image(systemName: "play.fill").foregroundColor(.white).shadow(radius: 2)
        }
      }
      .frame(width: 84, height: 84)
      .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
      .accessibilityLabel(file.kind == .video ? ShareStrings.t("itemVideo") : ShareStrings.t("itemPhoto"))
    } else {
      VStack(spacing: 6) {
        Image(systemName: "doc").font(.title3)
        Text(file.filename).font(.caption2).lineLimit(2).multilineTextAlignment(.center)
      }
      .padding(8)
      .frame(width: 84, height: 84)
      .background(theme.surface)
      .foregroundColor(theme.dim)
      .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
      .accessibilityLabel(ShareStrings.t("itemFile"))
    }
  }

  /// A small thumbnail: ImageIO downsamples from the file, so a 48 MP photo never decodes whole.
  static func thumbnail(_ file: ShareFile) -> UIImage? {
    switch file.kind {
    case .photo:
      guard let src = CGImageSourceCreateWithURL(file.url as CFURL, nil) else {
        return nil
      }
      let options: [CFString: Any] = [
        kCGImageSourceCreateThumbnailFromImageAlways: true,
        kCGImageSourceCreateThumbnailWithTransform: true,
        kCGImageSourceThumbnailMaxPixelSize: 252,
      ]
      return CGImageSourceCreateThumbnailAtIndex(src, 0, options as CFDictionary).map(UIImage.init(cgImage:))
    case .video:
      return file.poster.flatMap { Data(base64Encoded: $0) }.flatMap(UIImage.init(data:))
    case .file:
      return nil
    }
  }
}

struct FinishedView: View {
  @ObservedObject var model: ShareModel
  let theme: ShareTheme

  var body: some View {
    VStack(spacing: 12) {
      ScrollView {
        VStack(spacing: 2) {
          ForEach(model.selected) { target in
            HStack {
              Text(TargetRow.title(of: target)).lineLimit(1)
              Spacer()
              Text(label(model.outcomes[target.id] ?? "queued"))
                .font(.footnote)
                .foregroundColor(model.outcomes[target.id] == "rejected" ? theme.danger : theme.dim)
            }
            .padding(12)
          }
        }
      }
      if model.outcomes.values.contains("queued") {
        Text(ShareStrings.t("queued"))
          .font(.footnote)
          .foregroundColor(theme.dim)
          .multilineTextAlignment(.center)
          .accessibilityIdentifier("share-queued")
      }
      PrimaryButton(title: ShareStrings.t("done"), busy: false, enabled: true, theme: theme, action: model.done)
        .accessibilityIdentifier("share-done")
    }
    .padding(16)
  }

  private func label(_ status: String) -> String {
    switch status {
    case "sent":
      return ShareStrings.t("sent")
    case "rejected":
      return ShareStrings.t("rejected")
    default:
      return ShareStrings.t("sending")
    }
  }
}
