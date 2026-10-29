// #1360: a new device's approval request reaches the phone on its own; presented once per process. Pure.

export interface ApprovalCandidate {
  request_id: string;
  new_device_id: string;
}

export interface EnrollmentNudge extends ApprovalCandidate {
  link_id?: string | null;
}

/** Only inside the signed-in, unlocked app. */
export function canPresentApproval(segments: readonly string[], isLocked: boolean): boolean {
  if (!isInSignedInApp(segments, isLocked)) {
    return false;
  }
  return segments[0] !== "approve-device";
}

/** Past sign-in, enrollment and the PIN; also gates the inbox subscription. */
export function isInSignedInApp(segments: readonly string[], isLocked: boolean): boolean {
  if (isLocked || segments.length === 0) {
    return false;
  }
  const root = segments[0];
  return root !== "(auth)" && root !== "index";
}

/** A QR device link request (`link_id`) is approved on the other device, with no code to type. */
export function isTypedCodeRequest(nudge: EnrollmentNudge): boolean {
  return !nudge.link_id && !!nudge.request_id && !!nudge.new_device_id;
}

/** The newest request not yet presented in this process. */
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
