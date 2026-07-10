-------------------------------- MODULE CommitLog --------------------------------
(***************************************************************************)
(* Spec A -- CommitLog (invariants I1 + I2).                               *)
(*                                                                         *)
(* Machine-checked model of the Pollis MLS epoch / commit-log state        *)
(* machine, per `docs/machine-checked-correctness-design.md` section 3     *)
(* ("Spec A -- CommitLog").  This is an ABSTRACT model of the DESIGN, not  *)
(* the Rust: it is the design-level complement to the Kani proofs on the   *)
(* real pure functions (`pollis-delivery/src/commit.rs` `head_epoch_of` /  *)
(* `accepts`; `pollis-core/src/commands/mls/invariants.rs` `classify`).    *)
(* TLC checks it EXHAUSTIVELY over a small configuration (see              *)
(* CommitLog.cfg), so it catches the specific 3-way interleavings a random *)
(* proptest only samples -- a fork that manifests under one exact race at  *)
(* one epoch is a needle TLC always finds.                                 *)
(*                                                                         *)
(* This is the epoch/commit-log complement to Spec B (Delivery.tla): Spec  *)
(* A proves the log stays gapless / append-only / one-per-epoch under      *)
(* concurrent submits; Spec B proves retention never drops a message a     *)
(* current member still needs.                                             *)
(*                                                                         *)
(* ------------------------------------------------------------------------*)
(* STATE (design doc section 3, "Spec A").                                  *)
(*   log[k]          per-key ORDERED, append-only sequence of commits, each *)
(*                   a record [epoch |-> Nat, seq |-> Nat, author |-> c].   *)
(*                   `seq` is a globally-unique nonce (a fresh id per        *)
(*                   append) giving each commit a distinct byte-identity --  *)
(*                   the abstraction of the commit bytes used by            *)
(*                   NoForeignAdopt (I2).                                    *)
(*   localEpoch[k][c] client c's local epoch: how far it has applied the     *)
(*                   chain.  The next commit it expects is the one at epoch  *)
(*                   `localEpoch`.  Also the `based_on_epoch` it submits     *)
(*                   from.                                                   *)
(*   member[k][c]    I5 gate flag: TRUE = c is a current member of the tree. *)
(*                   Guards ExternalJoin (a removed/revoked device may NOT   *)
(*                   rejoin) and Submit (only a member commits).            *)
(*   adopted[k][c]   what c has ADOPTED at each epoch: a function            *)
(*                   [Epochs -> Seq-id or NoSeq].  adopted[k][c][e] is the   *)
(*                   `seq` (byte-identity) of the commit c installed at      *)
(*                   epoch e, or NoSeq if c never adopted an epoch-e commit. *)
(*                   Used only by NoForeignAdopt (I2).                       *)
(*   seqCtr          global monotone counter minting a unique `seq` per      *)
(*                   append (the commit-bytes nonce).                        *)
(*                                                                         *)
(* ------------------------------------------------------------------------*)
(* THE ABSTRACTION BOUNDARY (drift-mitigation rule, design doc section 3).  *)
(*   Submit(c)       <->  DS `submit_commit`                                 *)
(*                        (`pollis-delivery/src/commit.rs`): the atomic      *)
(*                        conditional-insert `WHERE ?2 = COALESCE(MAX(epoch),*)
(*                        -1)+1` -- append at the head IFF based_on = head,   *)
(*                        else reject.  `SoundSubmit` toggles that guard.     *)
(*   Apply(c) /      <->  client replay + gap detector                       *)
(*   ExternalJoin(c)      (`pollis-core/src/commands/mls/group_state.rs`      *)
(*                        `process_pending_commits_locked_impl`, via the      *)
(*                        Kani-proved `invariants::classify`): apply the      *)
(*                        next commit if present, else recover by jumping to  *)
(*                        the head (external join), gated by membership (I5). *)
(*                                                                         *)
(* FORWARD-COMPAT (design doc section 3 note, PQ hybrid MLS).  Everything is *)
(* keyed by an ABSTRACT `k \in Keys` -- the head/retention key -- rather     *)
(* than a hard-coded per-conversation epoch.  The PQ program extends the     *)
(* monotone key from (conversation, epoch) to (conversation, generation,     *)
(* epoch); with this parameterization that extension is a CONFIG change      *)
(* (enlarge `Keys` to model per-generation lineages), not a spec rewrite.    *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS
    Keys,        \* abstract head keys (a conversation, or a
                 \* (conversation, generation) pair once PQ lands)
    Clients,     \* the clients racing to submit / apply commits
    MaxCommits,  \* bound: total commit appends per key (K in the doc)
    MaxEpoch,    \* bound: highest epoch the group reaches
    SoundSubmit  \* TRUE  = submit accepts ONLY at the head (correct DS guard)
                 \* FALSE = broken variant: accept a stale based_on too (teeth --
                 \*         drops the conditional-insert check, forking the log)

VARIABLES
    log, localEpoch, member, adopted, seqCtr

vars == <<log, localEpoch, member, adopted, seqCtr>>

\* NoSeq (0) is the "never adopted" sentinel; real seqs are 1..MaxCommits.
NoSeq == 0

\* Epochs that can ever carry a commit (0 .. MaxEpoch-1). `adopted`'s domain.
Epochs == 0..(MaxEpoch - 1)

Max(S) == CHOOSE x \in S : \A y \in S : y <= x

----------------------------------------------------------------------------
(***************************************************************************)
(* Head arithmetic -- the abstraction of `head_epoch_of`                    *)
(* (`pollis-delivery/src/commit.rs`): head = MAX(epoch)+1, and 0 for an      *)
(* empty log (the SQL `COALESCE(MAX(epoch), -1) + 1`, whose transient -1     *)
(* never surfaces).                                                          *)
(***************************************************************************)
HeadEpoch(k) ==
    IF Len(log[k]) = 0
    THEN 0
    ELSE Max({ log[k][i].epoch : i \in DOMAIN log[k] }) + 1

\* The current head epoch of every key -- a state function of `log`, so its
\* primed form is well-defined for the HeadMonotone action property.
Heads == [k \in Keys |-> HeadEpoch(k)]

\* Is there a commit at epoch e in log[k]?
HasEpoch(k, e) == \E i \in DOMAIN log[k] : log[k][i].epoch = e

\* The commit record at epoch e (an arbitrary one if -- only under the broken
\* config -- two share an epoch; the sound spec keeps epochs unique).
CommitAt(k, e) == log[k][ CHOOSE i \in DOMAIN log[k] : log[k][i].epoch = e ]

----------------------------------------------------------------------------
Init ==
    /\ log        = [k \in Keys |-> << >>]
    /\ localEpoch = [k \in Keys |-> [c \in Clients |-> 0]]
    /\ member     = [k \in Keys |-> [c \in Clients |-> TRUE]]
    /\ adopted    = [k \in Keys |-> [c \in Clients |-> [e \in Epochs |-> NoSeq]]]
    /\ seqCtr     = 0

(***************************************************************************)
(* Submit(k, c) -- the DS `submit_commit` atomic conditional-insert         *)
(* (`pollis-delivery/src/commit.rs`).  Client c submits a commit based on    *)
(* its local epoch `b`:                                                      *)
(*   - IF b = HeadEpoch(k)  THEN append at the head (win the epoch) and c adopts   *)
(*     its own commit, advancing localEpoch to b+1.  This is the sole        *)
(*     accepting branch when SoundSubmit -- the `WHERE ?2 = head` guard.     *)
(*   - ELSE (b < Head) the real DS REJECTS (the conditional insert writes 0   *)
(*     rows); modelled by Submit simply not being enabled, so the stale       *)
(*     client must Apply / ExternalJoin to catch up before it can commit.     *)
(*     Concurrency = the interleaving of several clients' Submit steps racing  *)
(*     at one head: the first to fire wins and advances the head, leaving the  *)
(*     others stale (rejected) exactly as the serialized INSERT does.          *)
(* TEETH: SoundSubmit=FALSE drops the `b = Head` guard (accept any b <= Head), *)
(* so a stale client appends a SECOND commit at an already-occupied epoch --   *)
(* the fork the conditional-insert exists to prevent (OnePerEpoch violation).  *)
(***************************************************************************)
Submit(k, c) ==
    /\ member[k][c]                 \* only a current member commits
    /\ Len(log[k]) < MaxCommits     \* bound total appends
    /\ HeadEpoch(k) < MaxEpoch           \* bound the epoch range
    /\ LET b == localEpoch[k][c] IN
        /\ b <= HeadEpoch(k)             \* a client never bases on a future/unseen epoch
        /\ (SoundSubmit => b = HeadEpoch(k))   \* the conditional-insert guard (dropped by the teeth cfg)
        /\ LET nc == [epoch |-> b, seq |-> seqCtr + 1, author |-> c] IN
            /\ log' = [log EXCEPT ![k] = Append(@, nc)]
            /\ localEpoch' = [localEpoch EXCEPT ![k][c] = b + 1]
            /\ adopted' = [adopted EXCEPT ![k][c][b] = nc.seq]
    /\ seqCtr' = seqCtr + 1
    /\ UNCHANGED <<member>>

(***************************************************************************)
(* Apply(k, c) -- the client replay step                                    *)
(* (`process_pending_commits_locked_impl`, gated by the Kani-proved          *)
(* `invariants::classify`): if the NEXT commit (epoch = localEpoch) is        *)
(* present, apply it -- adopt it and advance the local epoch by one.  The     *)
(* `classify` decision is exactly "Apply iff this row's epoch == current      *)
(* epoch": there is never an Apply across a gap.  In this abstract log the    *)
(* chain is gapless by construction, so the next commit is present whenever   *)
(* the client is behind; the gap-recovery branch is subsumed by ExternalJoin. *)
(***************************************************************************)
Apply(k, c) ==
    /\ LET e == localEpoch[k][c] IN
        /\ e < MaxEpoch
        /\ HasEpoch(k, e)           \* classify => Apply: the exact next epoch exists
        /\ localEpoch' = [localEpoch EXCEPT ![k][c] = e + 1]
        /\ adopted' = [adopted EXCEPT ![k][c][e] = CommitAt(k, e).seq]
    /\ UNCHANGED <<log, member, seqCtr>>

(***************************************************************************)
(* ExternalJoin(k, c) -- the recovery jump                                  *)
(* (`external_join_group` reached from the gap branch of                     *)
(* `process_pending_commits_locked_impl`): a behind client abandons step-wise *)
(* replay and jumps its local epoch straight to the head.  GUARDED by         *)
(* member[k][c] (I5): a removed / revoked device can NOT rejoin the tree      *)
(* this way (fuzzer-finding-#2 leak).  The jumped-over epochs are NOT adopted *)
(* -- their commits (and any messages sealed at them) are an accepted loss    *)
(* (loss (a): history before you (re)joined), which is exactly why            *)
(* NoForeignAdopt stays true: the client records no commit it did not apply.  *)
(***************************************************************************)
ExternalJoin(k, c) ==
    /\ member[k][c]                        \* I5 gate
    /\ localEpoch[k][c] < HeadEpoch(k)          \* behind the head -> recover
    /\ localEpoch' = [localEpoch EXCEPT ![k][c] = HeadEpoch(k)]
    /\ UNCHANGED <<log, member, adopted, seqCtr>>

(***************************************************************************)
(* Remove(k, c) -- eviction: c stops being a current member.  Monotone       *)
(* (TRUE -> FALSE).  Models the F5/I5 boundary so the ExternalJoin gate is    *)
(* exercised: after a Remove, c can never take the recovery path back in.     *)
(***************************************************************************)
Remove(k, c) ==
    /\ member[k][c]
    /\ member' = [member EXCEPT ![k][c] = FALSE]
    /\ UNCHANGED <<log, localEpoch, adopted, seqCtr>>

Next ==
    \E k \in Keys :
        \E c \in Clients :
            \/ Submit(k, c)
            \/ Apply(k, c)
            \/ ExternalJoin(k, c)
            \/ Remove(k, c)

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(***************************************************************************)
(* INVARIANTS                                                               *)
(***************************************************************************)

TypeOK ==
    /\ seqCtr \in 0..MaxCommits
    /\ member \in [Keys -> [Clients -> BOOLEAN]]
    /\ localEpoch \in [Keys -> [Clients -> 0..MaxEpoch]]
    /\ adopted \in [Keys -> [Clients -> [Epochs -> 0..MaxCommits]]]
    /\ \A k \in Keys :
        /\ Len(log[k]) <= MaxCommits
        /\ \A i \in DOMAIN log[k] :
            /\ log[k][i].epoch \in Epochs
            /\ log[k][i].seq \in 1..MaxCommits
            /\ log[k][i].author \in Clients

\* I1(a) -- one commit per epoch: no two distinct log entries share an epoch.
\* The core anti-fork property the DS conditional-insert enforces.
OnePerEpoch ==
    \A k \in Keys :
        \A i, j \in DOMAIN log[k] :
            (log[k][i].epoch = log[k][j].epoch) => (i = j)

\* I1(b) -- gapless: the epochs present are exactly 0 .. Head-1, no hole.
\* With OnePerEpoch this means every epoch below the head appears exactly once.
Gapless ==
    \A k \in Keys :
        { log[k][i].epoch : i \in DOMAIN log[k] } = 0..(HeadEpoch(k) - 1)

\* I1(c) -- the head epoch never decreases (append-only, monotone). Action
\* property (robust under stuttering), like Spec B's CursorMonotone.
HeadMonotone ==
    [][ \A k \in Keys : Heads'[k] >= Heads[k] ]_vars

\* I2 -- no foreign adopt: every commit a client has adopted at epoch e
\* byte-equals the log's commit at e (abstracted as `seq` equality, seq being
\* the unique per-commit nonce). A client never installs a commit that is not
\* the one on the canonical log -- no phantom epoch, no fork adopted.
NoForeignAdopt ==
    \A k \in Keys : \A c \in Clients : \A e \in Epochs :
        (adopted[k][c][e] # NoSeq) =>
            \E i \in DOMAIN log[k] :
                /\ log[k][i].epoch = e
                /\ log[k][i].seq = adopted[k][c][e]

============================================================================
