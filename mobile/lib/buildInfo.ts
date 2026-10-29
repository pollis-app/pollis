// What this install is (#1443). Every value is native, so an OTA cannot change it.
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
  /** Null on the bundle the binary shipped with. */
  ota: { id: string; createdAt: Date | null } | null;
  destination: UpdateDestination;
};

// Deliberately not `Constants.expoConfig`, which an OTA replaces.
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

/** False when nothing could open. */
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
