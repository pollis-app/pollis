/// Whether an `enrollment_requested` nudge is one the typed-code approval can
/// complete. Twin of `mobile/lib/enrollmentPrompt.ts` → `isTypedCodeRequest`.
///
/// A request that belongs to a QR device link (`link_id`, #1207 / #1361) is
/// approved by the device at the other end of that QR — on the link tag or the
/// scanned key, never a typed code — and the new device shows no code to type.
/// Raising the typed-code takeover for it dead-ends the sign-in.
///
/// Skipping only withholds a prompt, which a DS can always do by not sending the
/// nudge; it approves nothing. `approve_device_enrollment` still requires the
/// typed code for any request, and a link request is wrapped only after the
/// core verifies its tag or key.
export function isTypedCodeRequest(nudge: {
  request_id?: string | null;
  new_device_id?: string | null;
  link_id?: string | null;
}): boolean {
  return !nudge.link_id && !!nudge.request_id && !!nudge.new_device_id;
}
