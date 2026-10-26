// Where an install updates from (#1443): the build's install channel, baked in
// by app.config.js (`extra.installChannel`), mapped to the place Self -> App ->
// Update opens. Pure — no react-native — so tests/install-channel.test.ts pins
// the mapping directly. The channel itself is read in lib/buildInfo.ts.

/** What app.config.js bakes into a build. */
export type InstallChannel = "store" | "sideload" | "dev";

export type UpdatePlatform = "ios" | "android";

/** Which place the Update row opens, by name. */
export type UpdateDestinationKind = "play" | "appstore" | "testflight" | "web";

export type UpdateDestination = {
  kind: UpdateDestinationKind;
  /** Opened first: the store app's own scheme where there is one. */
  url: string;
  /** Opened when `url` cannot be (no store app installed, scheme refused). */
  fallbackUrl?: string;
};

export const ANDROID_PACKAGE = "com.pollis.mobile";

// The App Store record's numeric Apple ID ("ascAppId", App Store Connect ->
// App Information). iOS is on TestFlight only until the App Store listing is
// live; set this then and the store channel opens the listing instead. It is
// JS, so an OTA update carries the change to every binary already shipped.
export const IOS_APP_STORE_ID: string | null = null;

/** The pollis.com page that offers the signed APK (and the Play link). */
export const SIDELOAD_PAGE_URL = "https://pollis.com/android";
export const WEBSITE_URL = "https://pollis.com/";

/**
 * The channel a build embedded, or null when it embedded none — every binary
 * built before #1443, which an OTA update can still bring this code to.
 */
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
  // One iOS binary serves TestFlight and (later) the App Store, so the build
  // cannot tell them apart; while there is no listing, TestFlight is where
  // every store install came from.
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
      // A binary from before the channel was baked in. iOS only ever shipped
      // through Apple; an Android one is Play or the APK, and the APK page
      // links to both, so it is right for either.
      return platform === "ios" ? appleDestination(appStoreId) : { kind: "web", url: SIDELOAD_PAGE_URL };
  }
}

/** A short form of an OTA update id for display: its first eight hex digits. */
export function shortUpdateId(updateId: string): string {
  return updateId.replace(/-/g, "").slice(0, 8).toLowerCase();
}
