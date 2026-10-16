// #1360: a new device of this account asking to be approved must reach the
// phone on its own, the way desktop's takeover does — not wait for the user to
// open Self → Security. Three things can surface one: the Delivery Service's
// live `enrollment_requested` nudge on the inbox room, the check that runs when
// the user lands in the signed-in app (sign-in, unlock, cold start), and the
// check that runs on every return to the foreground (the nudge is lost while
// the app is backgrounded or closed). Whichever fires, the request is
// presented once per process; backing out of the screen leaves it listed under
// Security instead of re-opening it on every resume.
//
// Pure (no react-native), so the node tests reach it.

/** The part of a pending request the approval screen is opened with. */
export interface ApprovalCandidate {
  request_id: string;
  new_device_id: string;
}

/** Live nudge payload, as `lib/realtime/events.ts` decodes it. */
export interface EnrollmentNudge extends ApprovalCandidate {
  link_id?: string | null;
}

/**
 * Whether the approval screen may be shown over the current route. Only inside
 * the signed-in, unlocked app: the auth group is where this device is itself
 * signing in or enrolling (it cannot approve anything there), and a locked
 * session must be unlocked first.
 */
export function canPresentApproval(segments: readonly string[], isLocked: boolean): boolean {
  if (!isInSignedInApp(segments, isLocked)) {
    return false;
  }
  // Already showing one.
  return segments[0] !== "approve-device";
}

/**
 * Whether the device is past sign-in, enrollment and the PIN: the auth group
 * and the boot redirect are not, and neither is a locked session. Also gates
 * the inbox realtime subscription, whose room token the device cannot get
 * before then.
 */
export function isInSignedInApp(segments: readonly string[], isLocked: boolean): boolean {
  if (isLocked || segments.length === 0) {
    return false;
  }
  const root = segments[0];
  return root !== "(auth)" && root !== "index";
}

/**
 * Whether a live nudge is one the typed-code approval can complete. A request
 * that belongs to a QR device link (`link_id`) is approved on the device at the
 * other end of that QR; the new device shows no code to type.
 */
export function isTypedCodeRequest(nudge: EnrollmentNudge): boolean {
  return !nudge.link_id && !!nudge.request_id && !!nudge.new_device_id;
}

/**
 * The request to open the approval screen for: the newest one not yet
 * presented in this process. The Delivery Service lists newest first.
 */
export function nextApproval(
  pending: readonly ApprovalCandidate[],
  presented: ReadonlySet<string>,
): ApprovalCandidate | null {
  for (const req of pending) {
    if (!presented.has(req.request_id)) {
      return req;
    }
  }
  return null;
}

type NudgeHandler = (nudge: ApprovalCandidate) => void;

let nudgeHandler: NudgeHandler | null = null;

/** Register what to do with a live nudge. Returns an unsubscribe. */
export function onEnrollmentRequested(next: NudgeHandler): () => void {
  nudgeHandler = next;
  return () => {
    if (nudgeHandler === next) {
      nudgeHandler = null;
    }
  };
}

/** Called by the inbox realtime handler with every `enrollment_requested`. */
export function noticeEnrollmentRequested(nudge: EnrollmentNudge): void {
  if (!isTypedCodeRequest(nudge) || !nudgeHandler) {
    return;
  }
  nudgeHandler({ request_id: nudge.request_id, new_device_id: nudge.new_device_id });
}
