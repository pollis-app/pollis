// Expo config plugin (#1685): the app-only iOS keychain access group for the biometric unlock key.
//
// The device unlock key (lib/deviceUnlock.ts) must be readable by the app and by nothing else. The
// app's DEFAULT keychain group is the first `keychain-access-groups` entry, which
// withNotificationServiceExtension makes `<team>.com.pollis.shared`, the group the Notification
// Service Extension reads with no user present. So this adds a second, app-only group
// (`<team>.com.pollis.app`) to the APP's entitlements only, never the extension's, and names it in
// the embedded config (`extra.deviceUnlock.iosAccessGroup`) for the store call to pass explicitly.
// Appended, never first: the default group, and every existing item, stay where they are. The
// team is spelled out for the same reason as the shared group (the unsigned store archive leaves
// `$(AppIdentifierPrefix)` empty). A `<team>.<suffix>` group needs no capability or profile change.
const { withEntitlementsPlist } = require("expo/config-plugins");

const APP_ONLY_SUFFIX = "com.pollis.app";

function appOnlyGroup(options) {
  const teamId = options?.teamId;
  if (!/^[A-Z0-9]{10}$/.test(teamId ?? "")) {
    throw new Error(`withDeviceUnlock needs a 10-character teamId option, got ${JSON.stringify(teamId)}`);
  }
  return `${teamId}.${APP_ONLY_SUFFIX}`;
}

/** The app's entitlements with the app-only group appended (kept last, never duplicated). */
function appEntitlements(entitlements, group) {
  const groups = (entitlements["keychain-access-groups"] ?? []).filter((g) => g !== group);
  return { ...entitlements, "keychain-access-groups": [...groups, group] };
}

const withDeviceUnlock = (config, options) => {
  const group = appOnlyGroup(options);
  // Read by lib/deviceUnlockRules.ts from the EMBEDDED config: an OTA bundle on an older binary
  // (no entitlement) finds no group and never offers biometric unlock.
  config.extra = {
    ...config.extra,
    deviceUnlock: { ...config.extra?.deviceUnlock, iosAccessGroup: group },
  };
  return withEntitlementsPlist(config, (c) => {
    c.modResults = appEntitlements(c.modResults, group);
    return c;
  });
};

module.exports = withDeviceUnlock;
module.exports.appOnlyGroup = appOnlyGroup;
module.exports.appEntitlements = appEntitlements;
module.exports.APP_ONLY_SUFFIX = APP_ONLY_SUFFIX;
