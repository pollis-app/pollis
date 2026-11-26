// Reads the app's biometric device unlock key (#1685) for the share extension (#2100).
//
// The app keeps it with expo-secure-store (`lib/deviceUnlock.ts`): a generic password, service
// "pollis.deviceUnlock:auth", account and generic attribute "duk.<user id>", in the app-only access
// group `<team>.com.pollis.app`, behind `.biometryCurrentSet`. This extension carries that group
// (a user is in front of it; the Notification Service Extension never does), so the system shows
// Face ID / Touch ID here exactly as in the app. Blocking: call off the main thread.

import Foundation
import LocalAuthentication
import Security

enum BiometricRead: Equatable {
  /// The base64 key.
  case key(String)
  /// The user cancelled or did not match: the keypad, nothing spent.
  case cancelled
  /// No item (biometrics re-enrolled, or the item is gone): the keypad with a note.
  case missing
}

enum BiometricKey {
  static let service = "pollis.deviceUnlock:auth"

  static func account(for userId: String) -> String? {
    guard !userId.isEmpty, userId.allSatisfy({ $0.isLetter || $0.isNumber || "._-".contains($0) }) else {
      return nil
    }
    return "duk.\(userId)"
  }

  static func read(userId: String, prompt: String) -> BiometricRead {
    guard let account = account(for: userId),
          let group = Bundle.main.object(forInfoDictionaryKey: "PollisAppOnlyKeychainGroup") as? String
    else {
      return .missing
    }
    let encoded = Data(account.utf8)
    let context = LAContext()
    context.localizedReason = prompt
    let query: [String: Any] = [
      kSecClass as String: kSecClassGenericPassword,
      kSecAttrService as String: service,
      kSecAttrAccount as String: encoded,
      kSecAttrGeneric as String: encoded,
      kSecAttrAccessGroup as String: group,
      kSecMatchLimit as String: kSecMatchLimitOne,
      kSecReturnData as String: true,
      kSecUseAuthenticationContext as String: context,
    ]
    var item: CFTypeRef?
    let status = SecItemCopyMatching(query as CFDictionary, &item)
    switch status {
    case errSecSuccess:
      guard let data = item as? Data, let key = String(data: data, encoding: .utf8), !key.isEmpty else {
        return .missing
      }
      return .key(key)
    case errSecItemNotFound:
      return .missing
    default:
      return .cancelled
    }
  }
}
