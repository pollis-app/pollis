/// Whether a nudge needs the typed-code takeover; QR-linked requests (#1207 / #1361) don't. Twin of `mobile/lib/enrollmentPrompt.ts`.
export function isTypedCodeRequest(nudge: {
  request_id?: string | null;
  new_device_id?: string | null;
  link_id?: string | null;
}): boolean {
  return !nudge.link_id && !!nudge.request_id && !!nudge.new_device_id;
}
