// Push capabilities this binary advertises with its token (#1599). Import-free for node tests.
//
// `rich`: the binary can decrypt a push itself — on iOS the Notification Service Extension, which
// plugins/withNotificationServiceExtension.js adds and records as `extra.richPush.ios`. The DS then
// marks that token's pushes mutable. Read from the binary's EMBEDDED config: an OTA update reaches
// binaries without the extension and must not claim it for them.

export type PushCap = "rich";

export function pushCapsFor(extra: unknown, platform: string): PushCap[] {
  if (!extra || typeof extra !== "object") {
    return [];
  }
  const richPush = (extra as { richPush?: unknown }).richPush;
  if (!richPush || typeof richPush !== "object") {
    return [];
  }
  return (richPush as Record<string, unknown>)[platform] === true ? ["rich"] : [];
}
