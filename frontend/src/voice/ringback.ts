/* #1901: the caller's ringback while a 1:1 DM call rings the other side. Decisions are pure so they are unit-tested. */

/** The slice of `appStore` the ringback reads. */
export interface RingbackSource {
  /** The outgoing call awaiting pickup. Only a DM call sets it, so group and channel calls never ring back. */
  outgoingCall: { callId: string } | null;
  voiceState: { kind: string; channelId?: string };
  voiceParticipants: ReadonlyArray<{ isLocal: boolean }>;
}

/**
 * Whether the caller should hear the ringback now: a DM call is ringing, this device is in (or joining)
 * that call's room, nobody else has joined it yet, and sound effects are on.
 *
 * Every way a ringing call ends clears `outgoingCall` or leaves the room, which stops the tone:
 * answered (the callee joins), declined or cancelled (`call_canceled`), timed out (`Call.tsx` leaves
 * after 30 s alone), hung up (leave), or the call screen closed (its backstop cancel).
 */
export function ringbackWanted(source: RingbackSource, soundEnabled: boolean): boolean {
  const { outgoingCall, voiceState, voiceParticipants } = source;
  if (!soundEnabled || !outgoingCall) {
    return false;
  }
  const inCallRoom =
    (voiceState.kind === "joining" || voiceState.kind === "joined") &&
    voiceState.channelId === `call-${outgoingCall.callId}`;
  if (!inCallRoom) {
    return false;
  }
  return !voiceParticipants.some((p) => !p.isLocal);
}

export interface RingbackOutput {
  start(): void;
  stop(): void;
}

/** Turns the wanted state into exactly one start per ring and one stop when it ends. */
export class Ringback {
  private playing = false;
  private readonly output: RingbackOutput;

  constructor(output: RingbackOutput) {
    this.output = output;
  }

  get isPlaying(): boolean {
    return this.playing;
  }

  sync(wanted: boolean): void {
    if (wanted === this.playing) {
      return;
    }
    this.playing = wanted;
    if (wanted) {
      this.output.start();
    } else {
      this.output.stop();
    }
  }
}
