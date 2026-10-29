/// Whether an `enrollment_requested` nudge is one the typed-code approval can
/// complete; twin of `mobile/lib/enrollmentPrompt.ts`. A QR-link request
/// (`link_id`, #1207 / #1361) shows no code, so the takeover would dead-end.
/// Skipping approves nothing: `approve_device_enrollment` still demands a code.
export function isTypedCodeRequest(nudge: {
  request_id?: string | null;
  new_device_id?: string | null;
  link_id?: string | null;
}): boolean {
  return !nudge.link_id && !!nudge.request_id && !!nudge.new_device_id;
}
