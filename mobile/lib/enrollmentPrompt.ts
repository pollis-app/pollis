// #1360: a new device's approval request must reach the phone on its own. The
// live `enrollment_requested` nudge, a check on landing in the signed-in app and
// one on every foreground can each surface it; it is presented once per
// process. Pure (no react-native), so the node tests reach it.

export interface ApprovalCandidate {
  request_id: string;
  new_device_id: string;
}

export interface EnrollmentNudge extends ApprovalCandidate {
  link_id?: string | null;
}

/**
 * Only inside the signed-in, unlocked app: in the auth group this device is
 * itself signing in and cannot approve anything.
 */
export function canPresentApproval(segments: readonly string[], isLocked: boolean): boolean {
  if (!isInSignedInApp(segments, isLocked)) {
    return false;
  }
  return segments[0] !== "approve-device";
}

/**
 * Past sign-in, enrollment and the PIN. Also gates the inbox realtime
 * subscription, whose room token the device cannot get before then.
 */
export function isInSignedInApp(segments: readonly string[], isLocked: boolean): boolean {
  if (isLocked || segments.length === 0) {
    return false;
  }
  const root = segments[0];
  return root !== "(auth)" && root !== "index";
}

/**
 * A QR device link's request (`link_id`) is approved at the other end of the
 * QR; the new device shows no code to type.
 */
export function isTypedCodeRequest(nudge: EnrollmentNudge): boolean {
  return !nudge.link_id && !!nudge.request_id && !!nudge.new_device_id;
}

/** The newest request not yet presented in this process (the DS lists newest first). */
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

export function onEnrollmentRequested(next: NudgeHandler): () => void {
  nudgeHandler = next;
  return () => {
    if (nudgeHandler === next) {
      nudgeHandler = null;
    }
  };
}

export function noticeEnrollmentRequested(nudge: EnrollmentNudge): void {
  if (!isTypedCodeRequest(nudge) || !nudgeHandler) {
    return;
  }
  nudgeHandler({ request_id: nudge.request_id, new_device_id: nudge.new_device_id });
}
