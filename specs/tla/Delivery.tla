-------------------------------- MODULE Delivery --------------------------------
(***************************************************************************)
(* Spec B -- Delivery / retention (invariants I3 + I4).                    *)
(*                                                                         *)
(* Machine-checked model of the Pollis delivery-watermark + commit/welcome *)
(* retention state machine, per                                            *)
(* `docs/machine-checked-correctness-design.md` sections 2 and 3           *)
(* ("Spec B -- Delivery").  This is an ABSTRACT model of the design, not   *)
(* the Rust: it is the design-level complement to the Kani proofs on the   *)
(* real `next_watermark`                                                    *)
(* (`pollis-core/src/commands/messages/watermark.rs`).  TLC checks it       *)
(* EXHAUSTIVELY over a small configuration (see Delivery.cfg), so it        *)
(* catches the specific interleavings a random proptest only samples.       *)
(*                                                                         *)
(* WHY THIS SPEC EXISTS NOW.  The commit-log retention floor (I4) is being *)
(* introduced (#539: "commit-log-retention-floor").  The design doc's rule  *)
(* is: model the floor BEFORE shipping the floor code, so the design is     *)
(* proved sound before it is written.  The teeth are the broken-GC config   *)
(* (DeliveryBroken.cfg): flip `SoundGC` to FALSE and TLC produces a real    *)
(* counterexample trace violating NoLossForCurrentMember.                   *)
(*                                                                         *)
(* ------------------------------------------------------------------------*)
(* STATE (design doc section 3, "Spec B").                                  *)
(*   msgs[k]        per-conversation ORDERED log of messages, each a record *)
(*                  [epoch |-> Nat, sentAt |-> Nat].  Append-only.          *)
(*   cursor[k][d]   per-(conversation, device) delivery watermark -- an     *)
(*                  EXCLUSIVE sentAt cursor (device has consumed everything  *)
(*                  with sentAt <= cursor).  Mirrors the value              *)
(*                  `next_watermark` returns.                               *)
(*   member[k]      the set of CURRENT member-devices of conversation k.    *)
(*   joinEpoch[k][d] the epoch at which d's CURRENT continuous presence     *)
(*                  began.  A leave+rejoin bumps this forward, which is      *)
(*                  exactly what makes "continuous presence" expressible as  *)
(*                  a single number (see AcceptedLossesOnly).               *)
(*   replay[k][d]   how far d has replayed the commit chain (its local      *)
(*                  epoch).  A message is decryptable once replay reaches    *)
(*                  its epoch; "un-handled" until then -- the watermark      *)
(*                  stop-at condition abstracted from `next_watermark`.      *)
(*   delivered[k][d] the set of sentAts d has actually DECRYPTED.  Used only *)
(*                  by AcceptedLossesOnly.                                   *)
(*   gcFloor[k]     retention floor: messages with sentAt <= gcFloor are     *)
(*                  removed (I4 retention GC).  EXCLUSIVE, like cursor.       *)
(*   clock          global monotone counter giving every Send a unique,     *)
(*                  strictly-increasing sentAt (models `sent_at` ordering).  *)
(*                                                                         *)
(* FORWARD-COMPAT (design doc section 3 note, PQ hybrid MLS).  Everything is *)
(* keyed by an ABSTRACT `k \in Keys` -- the head/retention key -- rather     *)
(* than a hard-coded per-conversation epoch.  The PQ program extends the     *)
(* monotone key from (conversation, epoch) to (conversation, generation,     *)
(* epoch); with this parameterization that extension is a CONFIG change      *)
(* (enlarge `Keys` to model per-generation lineages), not a spec rewrite.    *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Keys,      \* abstract head/retention keys (a conversation, or a
               \* (conversation, generation) pair once PQ lands)
    Devices,   \* member-devices, each with its own cursor
    MaxMsgs,   \* bound: messages per key (K in the doc, e.g. 4)
    MaxEpoch,  \* bound: highest epoch the group reaches
    SoundGC    \* TRUE  = retention floor guarded by the SLOWEST member (correct)
               \* FALSE = broken variant guarded by the FASTEST member (teeth)

VARIABLES
    clock, epoch, msgs, member, cursor, joinEpoch, replay, delivered, gcFloor

vars == <<clock, epoch, msgs, member, cursor, joinEpoch, replay, delivered, gcFloor>>

\* Largest sentAt any run can reach (one per Send, across all keys).
SentAtMax == MaxMsgs * Cardinality(Keys)

Max(S) == CHOOSE x \in S : \A y \in S : y <= x
Min(S) == CHOOSE x \in S : \A y \in S : y >= x

----------------------------------------------------------------------------
(***************************************************************************)
(* Watermark computation -- the abstraction boundary against the real       *)
(* `next_watermark` (pollis-core/src/commands/messages/watermark.rs).        *)
(*                                                                         *)
(* A message index i (in msgs[k]) is HANDLED for device d iff its epoch is   *)
(* either provably-never-reachable (below d's join -- an accepted loss) or   *)
(* already replayed (<= replay).  Otherwise it is UN-HANDLED and the         *)
(* watermark must stop strictly below it -- the anti-F3 no-skip rule.        *)
(***************************************************************************)

\* Present (not yet GC'd) and not-yet-consumed messages above d's cursor.
AboveCursor(k, d) ==
    { i \in DOMAIN msgs[k] :
        /\ msgs[k][i].sentAt > cursor[k][d]
        /\ msgs[k][i].sentAt > gcFloor[k] }

Handled(k, d, i) ==
    \/ msgs[k][i].epoch < joinEpoch[k][d]   \* pre-join: provably never reachable
    \/ msgs[k][i].epoch <= replay[k][d]     \* already replayed to this epoch

\* d can actually DECRYPT message i (not merely pass its cursor over it):
\* present continuously since its epoch AND replayed up to it.
Decryptable(k, d, i) ==
    /\ joinEpoch[k][d] <= msgs[k][i].epoch
    /\ msgs[k][i].epoch <= replay[k][d]

UnhandledAbove(k, d) == { i \in AboveCursor(k, d) : ~Handled(k, d, i) }

\* sentAt of the FIRST un-handled envelope above the cursor (SentAtMax+1 = +inf
\* sentinel when there is none). The watermark must stay strictly below this.
StopAt(k, d) ==
    IF UnhandledAbove(k, d) = {}
    THEN SentAtMax + 1
    ELSE Min({ msgs[k][i].sentAt : i \in UnhandledAbove(k, d) })

\* The maximal run of handled messages strictly below StopAt -- the candidates
\* the watermark may advance over (mirrors `next_watermark`'s candidate loop).
Candidates(k, d) ==
    { i \in AboveCursor(k, d) : msgs[k][i].sentAt < StopAt(k, d) }

NextWatermark(k, d) ==
    IF Candidates(k, d) = {}
    THEN cursor[k][d]
    ELSE Max({ msgs[k][i].sentAt : i \in Candidates(k, d) })

\* Of the messages the cursor now passes, those actually decrypted this step.
NewlyDecrypted(k, d) ==
    { msgs[k][i].sentAt : i \in { j \in Candidates(k, d) : Decryptable(k, d, j) } }

----------------------------------------------------------------------------
(* Retention floor bound (I4). Sound: never above the SLOWEST current        *)
(* member's cursor -- so a removed message was consumed by every member.     *)
(* Broken (SoundGC=FALSE): the FASTEST member's cursor -- removes messages a  *)
(* laggard still needs.  Empty roster => the floor may reach `clock`.         *)
GCBound(k) ==
    IF member[k] = {}
    THEN clock
    ELSE IF SoundGC
         THEN Min({ cursor[k][d] : d \in member[k] })
         ELSE Max({ cursor[k][d] : d \in member[k] })

----------------------------------------------------------------------------
Init ==
    /\ clock     = 0
    /\ epoch     = [k \in Keys |-> 0]
    /\ msgs      = [k \in Keys |-> << >>]
    /\ member    = [k \in Keys |-> {}]
    /\ cursor    = [k \in Keys |-> [d \in Devices |-> 0]]
    /\ joinEpoch = [k \in Keys |-> [d \in Devices |-> 0]]
    /\ replay    = [k \in Keys |-> [d \in Devices |-> 0]]
    /\ delivered = [k \in Keys |-> [d \in Devices |-> {}]]
    /\ gcFloor   = [k \in Keys |-> 0]

\* A commit advances the group epoch (bounded). Higher-epoch messages sent
\* after this are un-handled for any device that has not yet replayed to here.
Commit(k) ==
    /\ epoch[k] < MaxEpoch
    /\ epoch' = [epoch EXCEPT ![k] = @ + 1]
    /\ UNCHANGED <<clock, msgs, member, cursor, joinEpoch, replay, delivered, gcFloor>>

\* Send tags a message with the current epoch and a fresh unique sentAt.
Send(k) ==
    /\ Len(msgs[k]) < MaxMsgs
    /\ member[k] # {}
    /\ clock' = clock + 1
    /\ msgs'  = [msgs EXCEPT ![k] = Append(@, [epoch |-> epoch[k], sentAt |-> clock + 1])]
    /\ UNCHANGED <<epoch, member, cursor, joinEpoch, replay, delivered, gcFloor>>

\* A device joins at the current epoch. A brand-new (or rejoining) device
\* starts EMPTY: its cursor jumps to `clock` (it never fetches prior history)
\* and its delivered set is cleared. This is accepted loss (b) ("a new device
\* starts empty") and, for a rejoin, resets the continuous-presence interval.
Join(k, d) ==
    /\ d \notin member[k]
    /\ member'    = [member    EXCEPT ![k]    = @ \cup {d}]
    /\ joinEpoch' = [joinEpoch EXCEPT ![k][d] = epoch[k]]
    /\ replay'    = [replay    EXCEPT ![k][d] = epoch[k]]
    /\ cursor'    = [cursor    EXCEPT ![k][d] = clock]
    /\ delivered' = [delivered EXCEPT ![k][d] = {}]
    /\ UNCHANGED <<clock, epoch, msgs, gcFloor>>

\* A device leaves. It stops being a current member (so it no longer holds the
\* retention floor down); its cursor/joinEpoch/delivered are retained until a
\* possible rejoin. This is what lets the floor advance past a departed laggard.
Leave(k, d) ==
    /\ d \in member[k]
    /\ member' = [member EXCEPT ![k] = @ \ {d}]
    /\ UNCHANGED <<clock, epoch, msgs, cursor, joinEpoch, replay, delivered, gcFloor>>

\* A member replays one commit, advancing its local epoch toward the head.
\* This is what turns a previously un-handled (epoch-gated) message into a
\* decryptable one, unblocking the watermark.
ReplayCommit(k, d) ==
    /\ d \in member[k]
    /\ replay[k][d] < epoch[k]
    /\ replay' = [replay EXCEPT ![k][d] = @ + 1]
    /\ UNCHANGED <<clock, epoch, msgs, member, cursor, joinEpoch, delivered, gcFloor>>

\* The watermark step (abstracts `next_watermark`): advance the cursor over the
\* maximal handled prefix, stopping strictly below the first un-handled message,
\* and record the messages actually decrypted.
Advance(k, d) ==
    /\ d \in member[k]
    /\ NextWatermark(k, d) > cursor[k][d]
    /\ cursor'    = [cursor    EXCEPT ![k][d] = NextWatermark(k, d)]
    /\ delivered' = [delivered EXCEPT ![k][d] = @ \cup NewlyDecrypted(k, d)]
    /\ UNCHANGED <<clock, epoch, msgs, member, joinEpoch, replay, gcFloor>>

\* Retention GC (I4): raise the floor, removing messages at/below it. Guarded by
\* GCBound so (in the sound spec) it never passes a current member's cursor.
GC(k) ==
    /\ GCBound(k) > gcFloor[k]
    /\ \E f \in (gcFloor[k] + 1)..GCBound(k) :
           gcFloor' = [gcFloor EXCEPT ![k] = f]
    /\ UNCHANGED <<clock, epoch, msgs, member, cursor, joinEpoch, replay, delivered>>

Next ==
    \E k \in Keys :
        \/ Commit(k)
        \/ Send(k)
        \/ GC(k)
        \/ \E d \in Devices :
               \/ Join(k, d)
               \/ Leave(k, d)
               \/ ReplayCommit(k, d)
               \/ Advance(k, d)

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(***************************************************************************)
(* INVARIANTS                                                               *)
(***************************************************************************)

TypeOK ==
    /\ clock \in 0..SentAtMax
    /\ epoch \in [Keys -> 0..MaxEpoch]
    /\ member \in [Keys -> SUBSET Devices]
    /\ cursor \in [Keys -> [Devices -> 0..SentAtMax]]
    /\ joinEpoch \in [Keys -> [Devices -> 0..MaxEpoch]]
    /\ replay \in [Keys -> [Devices -> 0..MaxEpoch]]
    /\ gcFloor \in [Keys -> 0..SentAtMax]

\* Sanity: a cursor is always a real (past) sentAt bound. Makes CursorMonotone's
\* Join case (cursor := clock) provably non-regressing.
CursorLeqClock ==
    \A k \in Keys : \A d \in Devices : cursor[k][d] <= clock

\* A current member still NEEDS message m if it has not consumed it (sentAt >
\* cursor) and is entitled to it (joined at/before its epoch -- not a pre-join
\* accepted loss).
Needed(k, d, i) ==
    /\ msgs[k][i].sentAt > cursor[k][d]
    /\ joinEpoch[k][d] <= msgs[k][i].epoch

\* I3 + I4, the anti-F3 property: retention never removes a message a current
\* member-device still needs. GC below the slowest member is safe; GC past it
\* (the broken variant) drops a message a laggard has not delivered.
NoLossForCurrentMember ==
    \A k \in Keys : \A d \in member[k] : \A i \in DOMAIN msgs[k] :
        Needed(k, d, i) => msgs[k][i].sentAt > gcFloor[k]

\* Cursors never regress (temporal / action property; robust under stuttering).
CursorMonotone ==
    [][ \A k \in Keys : \A d \in Devices : cursor'[k][d] >= cursor[k][d] ]_vars

\* The two accepted losses and NOTHING WEAKER: a device has decrypted m only if
\* it was continuously present since m's epoch (joinEpoch <= m.epoch, where any
\* leave+rejoin resets joinEpoch). Excludes pre-join messages (accepted loss a)
\* and a fresh device's prior history (accepted loss b); a device that was NOT
\* continuously present can never appear to have decrypted m.
AcceptedLossesOnly ==
    \A k \in Keys : \A d \in Devices : \A i \in DOMAIN msgs[k] :
        (msgs[k][i].sentAt \in delivered[k][d]) => (joinEpoch[k][d] <= msgs[k][i].epoch)

============================================================================
