// What this install is (#1443): the native version and build number, the OTA
// update it is running (if any), and the install channel the BINARY was built
// for. Every value comes from the native side, so an OTA update — which
// replaces the JS and its copy of the app config — cannot change any of them.
import { Linking, Platform } from "react-native";
import { requireOptionalNativeModule } from "expo";
import * as Application from "expo-application";
import * as Updates from "expo-updates";
import {
  parseInstallChannel,
  updateDestination,
  type InstallChannel,
  type UpdateDestination,
} from "./installChannel";

export type BuildInfo = {
  version: string | null;
  build: string | null;
  channel: InstallChannel | null;
  /** The running OTA update, or null on the bundle the binary shipped with. */
  ota: { id: string; createdAt: Date | null } | null;
  destination: UpdateDestination;
};

// The app config expo-constants embedded at native build time (APK/AAB
// `assets/app.config`, IPA `EXConstants.bundle/app.config`). Deliberately not
// `Constants.expoConfig`: once an OTA update runs, that is the UPDATE's config,
// built by the OTA job, which knows nothing about the binary's channel.
function embeddedInstallChannel(): InstallChannel | null {
  const native = requireOptionalNativeModule<{ manifest?: unknown }>("ExponentConstants");
  let config = native?.manifest;
  if (typeof config === "string") {
    try {
      config = JSON.parse(config);
    } catch {
      return null;
    }
  }
  const extra = (config as { extra?: { installChannel?: unknown } } | null | undefined)?.extra;
  return parseInstallChannel(extra?.installChannel);
}

function runningUpdate(): BuildInfo["ota"] {
  if (!Updates.isEnabled || Updates.isEmbeddedLaunch || !Updates.updateId) {
    return null;
  }
  return { id: Updates.updateId, createdAt: Updates.createdAt };
}

let cached: BuildInfo | null = null;

/** Constant for the life of the process, so it is read once. */
export function getBuildInfo(): BuildInfo {
  if (cached) {
    return cached;
  }
  const channel = embeddedInstallChannel();
  const platform = Platform.OS === "ios" ? "ios" : "android";
  cached = {
    version: Application.nativeApplicationVersion,
    build: Application.nativeBuildVersion,
    channel,
    ota: runningUpdate(),
    destination: updateDestination(channel, platform, {
      androidPackage: Platform.OS === "android" ? Application.applicationId : null,
    }),
  };
  return cached;
}

/** Open where this install updates from; false when nothing could open. */
export async function openUpdateDestination(dest: UpdateDestination): Promise<boolean> {
  try {
    await Linking.openURL(dest.url);
    return true;
  } catch {
    if (!dest.fallbackUrl) {
      return false;
    }
  }
  try {
    await Linking.openURL(dest.fallbackUrl);
    return true;
  } catch {
    return false;
  }
}
