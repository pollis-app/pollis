// The extension's copy (#2100): `mobile:share.*` from the shared catalogues, written per .lproj as
// Localizable.strings by plugins/withShareExtension.js, so it follows the iOS language. i18next
// placeholders arrive as positional `%1$@`; plurals as `<key>_one` / `<key>_other`.

import Foundation

enum ShareStrings {
  /// The bundle the strings live in (the harness points this at its own).
  static var bundle: Bundle = .main

  static func t(_ key: String) -> String {
    bundle.localizedString(forKey: key, value: nil, table: nil)
  }

  static func t(_ key: String, _ args: String...) -> String {
    String(format: t(key), arguments: args.map { $0 as CVarArg })
  }

  /// `<key>_one` for exactly one when the language has it, else `<key>_other`; `%1$@` = count.
  static func plural(_ key: String, _ count: Int) -> String {
    let one = "\(key)_one"
    let format = count == 1 && t(one) != one ? t(one) : t("\(key)_other")
    return String(format: format, String(count))
  }
}
