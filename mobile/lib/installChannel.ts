// Where an install updates from (#1443): the channel app.config.js bakes in
// (`extra.installChannel`, read in lib/buildInfo.ts) mapped to what Self -> App
// -> Update opens. Pure, so the node tests pin the mapping.

export type InstallChannel = "store" | "sideload" | "dev";

export type UpdatePlatform = "ios" | "android";

export type UpdateDestinationKind = "play" | "appstore" | "testflight" | "web";

export type UpdateDestination = {
  kind: UpdateDestinationKind;
  /** Opened first: the store app's own scheme where there is one. */
  url: string;
  /** Opened when `url` cannot be (no store app installed, scheme refused). */
  fallbackUrl?: string;
};

export const ANDROID_PACKAGE = "com.pollis.mobile";

// The App Store record's numeric Apple ID ("ascAppId"). Null while iOS is on
// TestFlight only; set it when the listing is live (JS, so an OTA carries it).
export const IOS_APP_STORE_ID: string | null = null;

export const SIDELOAD_PAGE_URL = "https://pollis.com/android";
export const WEBSITE_URL = "https://pollis.com/";

/** Null for a binary built before #1443, which an OTA can still bring this code to. */
export function parseInstallChannel(value: unknown): InstallChannel | null {
  if (value === "store" || value === "sideload" || value === "dev") {
    return value;
  }
  return null;
}

function playListing(androidPackage: string): UpdateDestination {
  return {
    kind: "play",
    url: `market://details?id=${androidPackage}`,
    fallbackUrl: `https://play.google.com/store/apps/details?id=${androidPackage}`,
  };
}

function appleDestination(appStoreId: string | null): UpdateDestination {
  if (appStoreId) {
    return {
      kind: "appstore",
      url: `itms-apps://apps.apple.com/app/id${appStoreId}`,
      fallbackUrl: `https://apps.apple.com/app/id${appStoreId}`,
    };
  }
  // One iOS binary serves TestFlight and the App Store, so the build cannot
  // tell them apart; with no listing yet, TestFlight is the source.
  return { kind: "testflight", url: "itms-beta://", fallbackUrl: "https://testflight.apple.com/" };
}

export function updateDestination(
  channel: InstallChannel | null,
  platform: UpdatePlatform,
  opts: { androidPackage?: string | null; appStoreId?: string | null } = {},
): UpdateDestination {
  const androidPackage = opts.androidPackage || ANDROID_PACKAGE;
  const appStoreId = opts.appStoreId === undefined ? IOS_APP_STORE_ID : opts.appStoreId;
  switch (channel) {
    case "store":
      return platform === "android" ? playListing(androidPackage) : appleDestination(appStoreId);
    case "sideload":
      return { kind: "web", url: SIDELOAD_PAGE_URL };
    case "dev":
      return { kind: "web", url: WEBSITE_URL };
    case null:
      // Pre-#1443 binary. iOS only shipped through Apple; for Android the APK
      // page links both Play and the APK, so it suits either.
      return platform === "ios" ? appleDestination(appStoreId) : { kind: "web", url: SIDELOAD_PAGE_URL };
  }
}

export function shortUpdateId(updateId: string): string {
  return updateId.replace(/-/g, "").slice(0, 8).toLowerCase();
}
