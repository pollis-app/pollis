// Expo config plugin (#1599): makes pollis-native's PollisMessagingService the app's only FCM service.
// It subclasses expo-notifications' ExpoFirebaseMessagingService (and calls super for every push
// that is not a rich one), so expo's own <service> is removed here, in the app manifest, which wins
// every manifest merge, rather than relying on intent-filter priority between two library services.
const { withAndroidManifest } = require('expo/config-plugins');

const EXPO_SERVICE = 'expo.modules.notifications.service.ExpoFirebaseMessagingService';
const TOOLS_NS = 'http://schemas.android.com/tools';

function removeExpoMessagingService(manifest) {
  manifest.$ = manifest.$ ?? {};
  manifest.$['xmlns:tools'] = manifest.$['xmlns:tools'] ?? TOOLS_NS;
  const app = manifest.application?.[0];
  if (!app) {
    throw new Error('withRichPush: AndroidManifest.xml has no <application>');
  }
  app.service = app.service ?? [];
  const already = app.service.some((s) => s.$?.['android:name'] === EXPO_SERVICE);
  if (!already) {
    app.service.push({ $: { 'android:name': EXPO_SERVICE, 'tools:node': 'remove' } });
  }
  return manifest;
}

// Embedded in the binary's expo config: only a binary that carries the native handler asks the DS
// for data-only pushes (`lib/push/caps.ts`), so an OTA bundle on an older binary never does.
function markRichPush(config) {
  config.extra = config.extra ?? {};
  config.extra.richPush = { ...(config.extra.richPush ?? {}), android: true };
  return config;
}

const withRichPush = (config) =>
  withAndroidManifest(markRichPush(config), (config) => {
    removeExpoMessagingService(config.modResults.manifest);
    return config;
  });

module.exports = withRichPush;
module.exports.removeExpoMessagingService = removeExpoMessagingService;
module.exports.markRichPush = markRichPush;
module.exports.EXPO_SERVICE = EXPO_SERVICE;
