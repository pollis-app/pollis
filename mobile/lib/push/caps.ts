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

// "dm_request" (#1734): this binary knows the `dm_request` push kind — its core resolves it to
// "Message request" + the requester's name, its native handlers show that, and its JS routes a
// tap to the request list. The DS then pushes a new request, and a request's first message under
// that kind. Asked for only when the embedded `extra.dmRequestPush === true` (app.json), so a
// binary from before #1734 keeps today's pushes.
export const DM_REQUEST_PUSH_CAP = "dm_request";

export function pushCapsFor(embeddedExtra: unknown, platform: string): string[] {
  if (typeof embeddedExtra !== "object" || embeddedExtra === null) {
    return [];
  }
  const caps: string[] = [];
  const rich = (embeddedExtra as { richPush?: unknown }).richPush;
  if (typeof rich === "object" && rich !== null && (rich as Record<string, unknown>)[platform] === true) {
    caps.push(RICH_PUSH_CAP);
  }
  if ((embeddedExtra as { dmRequestPush?: unknown }).dmRequestPush === true) {
    caps.push(DM_REQUEST_PUSH_CAP);
  }
  return caps;
}
