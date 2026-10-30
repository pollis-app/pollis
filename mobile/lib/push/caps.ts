// What this binary's push handling can do, sent with the token (#1599). Import-free for node tests.
//
// "rich": the device decrypts and rewrites the notification itself, so the DS may send iOS
// `mutableContent` (the Notification Service Extension) and Android a data-only `{h}` (the native
// PollisMessagingService). Asked for only when the binary's EMBEDDED expo config says the native
// half is there (`extra.richPush[platform] === true`, set by that platform's config plugin): an
// OTA bundle on an older binary reads the old binary's config and so never asks for pushes it
// cannot show. Callers pass the embedded config's `extra`, never `Constants.expoConfig` (which an
// OTA update replaces).

export const RICH_PUSH_CAP = "rich";

export function pushCapsFor(embeddedExtra: unknown, platform: string): string[] {
  if (typeof embeddedExtra !== "object" || embeddedExtra === null) {
    return [];
  }
  const rich = (embeddedExtra as { richPush?: unknown }).richPush;
  if (typeof rich !== "object" || rich === null) {
    return [];
  }
  return (rich as Record<string, unknown>)[platform] === true ? [RICH_PUSH_CAP] : [];
}
