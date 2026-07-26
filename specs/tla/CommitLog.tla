-------------------------------- MODULE CommitLog --------------------------------
(***************************************************************************)
(* Spec A -- CommitLog (invariants I1 + I2), widened to suite lineages.     *)
(*                                                                         *)
(* Machine-checked model of the Pollis MLS epoch / commit-log state        *)
(* machine, per `docs/machine-checked-correctness-design.md` section 3     *)
(* ("Spec A -- CommitLog").  This is an ABSTRACT model of the DESIGN, not  *)
(* the Rust: it is the design-level complement to the Kani proofs on the   *)
(* real pure functions (`pollis-delivery/src/commit.rs` `head_epoch_of` /  *)
(* `head_generation` / `accepts`; `pollis-core/src/commands/mls/           *)
(* invariants.rs` `classify`).  TLC checks it EXHAUSTIVELY over a small    *)
(* configuration (see CommitLog.cfg), so it catches the specific 3-way     *)
(* interleavings a random proptest only samples -- a fork that manifests   *)
(* under one exact race at one epoch is a needle TLC always finds.         *)
(*                                                                         *)
(* This is the epoch/commit-log complement to Spec B (Delivery.tla): Spec  *)
(* A proves the log stays gapless / append-only / one-per-epoch under      *)
(* concurrent submits; Spec B proves retention never drops a message a     *)
(* current member still needs.                                             *)
(*                                                                         *)
(* ------------------------------------------------------------------------*)
(* SUITE GENERATIONS (issue #454 P4).                                       *)
(*                                                                          *)
(* MLS binds the ciphersuite into the group at creation and offers no       *)
(* in-band way to change it, so migrating a live conversation to the        *)
(* post-quantum hybrid suite cannot be a commit.  It is a SUCCESSOR GROUP:  *)
(* a second MLS group for the same conversation, whose roster is moved      *)
(* across by Welcome, and whose epoch counter RESTARTS AT 0.                *)
(*                                                                          *)
(* The per-conversation monotone key therefore widens from `epoch` to       *)
(* `(generation, epoch)`, ordered lexicographically.  An earlier draft of   *)
(* this spec claimed that extension would be "a CONFIG change (enlarge      *)
(* `Keys`), not a spec rewrite".  That is FALSE, and modelling it that way  *)
(* would have been a hollow gesture: two unrelated keys model two unrelated *)
(* conversations, not a conversation and its successor.  The whole safety   *)
(* content of P4 is the RELATIONSHIP between the lineages -- specifically,  *)
(* that opening a lineage is a COMPARE-AND-SWAP on the closing one, so no   *)
(* commit can be orphaned into a lineage that has just been retired.  That  *)
(* relationship needs actions, not constants, which is why the migration is *)
(* modelled here as a two-step prepare/commit with an explicit race window. *)
(*                                                                          *)
(* Generation 0 is every conversation that exists today.  With `MaxGen = 0` *)
(* no migration is ever enabled and this module degenerates, action for     *)
(* action, into the pre-P4 spec.                                            *)
(*                                                                          *)
(* ------------------------------------------------------------------------*)
(* STATE (design doc section 3, "Spec A").                                  *)
(*   log[k]          per-key ORDERED, append-only sequence of commits, each *)
(*                   a record [gen |-> Nat, epoch |-> Nat, seq |-> Nat,     *)
(*                   author |-> c, closes |-> Nat].  `seq` is a globally-   *)
(*                   unique nonce (a fresh id per append) giving each       *)
(*                   commit a distinct byte-identity -- the abstraction of  *)
(*                   the commit bytes used by NoForeignAdopt (I2).          *)
(*                   `closes` is meaningful only on the OPENING commit of a *)
(*                   generation (`epoch = 0 /\ gen > 0`): it is the head    *)
(*                   epoch of the predecessor lineage that the migrator     *)
(*                   observed and thereby declared closed.  NoClose on      *)
(*                   every ordinary commit.                                 *)
(*   localGen[k][c] which lineage client c currently reads and writes.      *)
(*   localEpoch[k][c] c's local epoch WITHIN localGen: how far it has       *)
(*                   applied that lineage's chain.  The next commit it      *)
(*                   expects is the one at epoch `localEpoch`.  Also the    *)
(*                   `based_on_epoch` it submits from.                      *)
(*   member[k][c]    I5 gate flag: TRUE = c is a current member of the tree.*)
(*                   Guards ExternalJoin (a removed/revoked device may NOT  *)
(*                   rejoin) and Submit (only a member commits).            *)
(*   adopted[k][c]   what c has ADOPTED at each (generation, epoch): a      *)
(*                   function [Gens -> [Epochs -> Seq-id or NoSeq]].        *)
(*                   Used only by NoForeignAdopt (I2).                      *)
(*   pending[k][c]   c's in-flight migration: the (generation, head epoch)  *)
(*                   pair it OBSERVED, which its opening commit will name   *)
(*                   as `closes`.  NoPend when c has no migration staged.   *)
(*                   This is the whole point of the two-step model -- the   *)
(*                   gap between reading the head and writing the successor *)
(*                   is the race the compare-and-swap has to survive.       *)
(*   seqCtr          global monotone counter minting a unique `seq` per     *)
(*                   append (the commit-bytes nonce).                       *)
(*                                                                         *)
(* ------------------------------------------------------------------------*)
(* THE ABSTRACTION BOUNDARY (drift-mitigation rule, design doc section 3).  *)
(*   Submit(c)       <->  DS `submit_commit`                                *)
(*                        (`pollis-delivery/src/commit.rs`): the atomic     *)
(*                        conditional-insert `WHERE ?2 = COALESCE(MAX(epoch)*)
(*                        , -1)+1` within one generation -- append at the   *)
(*                        head IFF based_on = head, else reject.            *)
(*                        `SoundSubmit` toggles that guard.                 *)
(*   MigratePrepare  <->  `migrate_to_hybrid_if_due` steps 4-7             *)
(*   / MigrateCommit       (`pollis-core/src/commands/mls/migrate.rs`):     *)
(*   / MigrateAbandon       read the predecessor's head, stage the          *)
(*                        successor's opening commit, publish it naming     *)
(*                        that head as `closes_epoch`, and roll the         *)
(*                        successor back if the DS refuses.  The DS side is *)
(*                        the second branch of the Kani-proved `accepts()`. *)
(*                        `SoundMigrate` toggles the compare-and-swap.      *)
(*   AdoptGeneration <->  a member moved across by Welcome                  *)
(*                        (`apply_welcome` + `maybe_advance_generation`):   *)
(*                        DRAIN the current lineage first, then hop, one    *)
(*                        generation at a time.  Draining first is not      *)
(*                        tidiness: with `max_past_epochs = 0` a message    *)
(*                        must be decrypted while its group is still at its *)
(*                        epoch, so adopting early destroys the only key    *)
(*                        material that can read the predecessor's backlog. *)
(*   Apply(c) /      <->  client replay + gap detector                      *)
(*   ExternalJoin(c)      (`pollis-core/src/commands/mls/group_state.rs`    *)
(*                        `process_pending_commits_locked_impl`, via the    *)
(*                        Kani-proved `invariants::classify`): apply the    *)
(*                        next commit if present, else recover by jumping   *)
(*                        to the head (external join), gated by membership. *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS
    Keys,        \* conversations (the head/retention key)
    Clients,     \* the clients racing to submit / apply / migrate
    MaxCommits,  \* bound: total commit appends per key (K in the doc)
    MaxEpoch,    \* bound: highest epoch a lineage reaches
    MaxGen,      \* bound: highest suite generation (0 = pre-P4 behaviour)
    SoundSubmit, \* TRUE  = submit accepts ONLY at the head (correct DS guard)
                 \* FALSE = broken variant: accept a stale based_on too (teeth --
                 \*         drops the conditional-insert check, forking the log)
    SoundMigrate \* TRUE  = opening a lineage is a compare-and-swap on the
                 \*         predecessor's head (correct DS guard)
                 \* FALSE = broken variant: open on a STALE observed head (teeth --
                 \*         orphans any commit that landed in the race window)

VARIABLES
    log, localGen, localEpoch, member, adopted, seqCtr, pending

vars == <<log, localGen, localEpoch, member, adopted, seqCtr, pending>>

\* NoSeq (0) is the "never adopted" sentinel; real seqs are 1..MaxCommits.
NoSeq == 0

\* `closes` on an ordinary (non-opening) commit, and the empty `pending` slot.
NoClose == -1
NoPend == [gen |-> -1, epoch |-> -1]

\* Epochs that can ever carry a commit (0 .. MaxEpoch-1). `adopted`'s domain.
Epochs == 0..(MaxEpoch - 1)

\* Suite generations. `MaxGen = 0` disables migration entirely.
Gens == 0..MaxGen

Max(S) == CHOOSE x \in S : \A y \in S : y <= x

----------------------------------------------------------------------------
(***************************************************************************)
(* Lineage arithmetic.                                                      *)
(*                                                                          *)
(* HeadGen  <-> `head_generation` (`COALESCE(MAX(generation), 0)`): an empty *)
(*              log is generation 0, the lineage a brand-new conversation    *)
(*              starts in.                                                   *)
(* HeadEpochIn <-> `head_epoch_of` scoped to one lineage: MAX(epoch)+1 over  *)
(*              that generation's rows, and 0 when it has none (the SQL      *)
(*              `COALESCE(MAX(epoch), -1) + 1`, whose transient -1 never     *)
(*              surfaces).  Scoping to a lineage is the widening: a          *)
(*              successor's epoch 0 is not "behind" the predecessor's last   *)
(*              epoch, it is AHEAD of it.                                    *)
(***************************************************************************)
HeadGen(k) ==
    IF Len(log[k]) = 0
    THEN 0
    ELSE Max({ log[k][i].gen : i \in DOMAIN log[k] })

\* Indices of log[k] belonging to generation g.
IdxIn(k, g) == { i \in DOMAIN log[k] : log[k][i].gen = g }

HeadEpochIn(k, g) ==
    LET E == { log[k][i].epoch : i \in IdxIn(k, g) } IN
    IF E = {} THEN 0 ELSE Max(E) + 1

\* The conversation's monotone key, as a lexicographic pair. A state function
\* of `log`, so its primed form is well-defined for the LineageMonotone action
\* property.
Lineage == [k \in Keys |-> << HeadGen(k), HeadEpochIn(k, HeadGen(k)) >>]

\* Is there a commit at (g, e) in log[k]?
HasEpoch(k, g, e) == \E i \in IdxIn(k, g) : log[k][i].epoch = e

\* The commit record at (g, e) (an arbitrary one if -- only under a broken
\* config -- two share it; the sound spec keeps the pair unique).
CommitAt(k, g, e) == log[k][ CHOOSE i \in IdxIn(k, g) : log[k][i].epoch = e ]

----------------------------------------------------------------------------
Init ==
    /\ log        = [k \in Keys |-> << >>]
    /\ localGen   = [k \in Keys |-> [c \in Clients |-> 0]]
    /\ localEpoch = [k \in Keys |-> [c \in Clients |-> 0]]
    /\ member     = [k \in Keys |-> [c \in Clients |-> TRUE]]
    /\ adopted    = [k \in Keys |->
                        [c \in Clients |->
                            [g \in Gens |-> [e \in Epochs |-> NoSeq]]]]
    /\ pending    = [k \in Keys |-> [c \in Clients |-> NoPend]]
    /\ seqCtr     = 0

(***************************************************************************)
(* Submit(k, c) -- the DS `submit_commit` atomic conditional-insert         *)
(* (`pollis-delivery/src/commit.rs`).  Client c submits a commit into its   *)
(* OWN lineage, based on its local epoch `b`:                               *)
(*   - IF b = HeadEpochIn(k, g) THEN append at that lineage's head and c    *)
(*     adopts its own commit.  This is the sole accepting branch when       *)
(*     SoundSubmit -- the `WHERE ?2 = head` guard.                          *)
(*   - ELSE (b < Head) the real DS REJECTS (the conditional insert writes 0 *)
(*     rows); modelled by Submit simply not being enabled, so the stale     *)
(*     client must Apply / ExternalJoin to catch up before it can commit.   *)
(*     Concurrency = the interleaving of several clients' Submit steps      *)
(*     racing at one head: the first to fire wins and advances the head,    *)
(*     leaving the others stale (rejected) exactly as the serialized INSERT *)
(*     does.                                                                *)
(*                                                                          *)
(* `localGen[k][c] = HeadGen(k)` is the P4 addition: a device still on a    *)
(* RETIRED lineage cannot write to it.  The DS enforces this in the same    *)
(* atomic step (`accepts` takes only two branches -- continue the head      *)
(* lineage, or open the next one), so as with a stale based_on, rejection   *)
(* is modelled by the action not being enabled.                             *)
(*                                                                          *)
(* TEETH: SoundSubmit=FALSE drops the `b = Head` guard (accept any b <= Head)*)
(* so a stale client appends a SECOND commit at an already-occupied epoch -- *)
(* the fork the conditional-insert exists to prevent (OnePerEpoch violation).*)
(***************************************************************************)
Submit(k, c) ==
    /\ member[k][c]                 \* only a current member commits
    /\ Len(log[k]) < MaxCommits     \* bound total appends
    /\ localGen[k][c] = HeadGen(k)  \* a retired lineage accepts no commit
    /\ LET g == localGen[k][c]
           b == localEpoch[k][c] IN
        /\ HeadEpochIn(k, g) < MaxEpoch      \* bound the epoch range
        /\ b <= HeadEpochIn(k, g)            \* never base on a future/unseen epoch
        /\ (SoundSubmit => b = HeadEpochIn(k, g))  \* the conditional-insert guard
        /\ LET nc == [gen |-> g, epoch |-> b, seq |-> seqCtr + 1,
                      author |-> c, closes |-> NoClose] IN
            /\ log' = [log EXCEPT ![k] = Append(@, nc)]
            /\ localEpoch' = [localEpoch EXCEPT ![k][c] = b + 1]
            /\ adopted' = [adopted EXCEPT ![k][c][g][b] = nc.seq]
    /\ seqCtr' = seqCtr + 1
    /\ UNCHANGED <<member, localGen, pending>>

(***************************************************************************)
(* MigratePrepare(k, c) -- the migrator READS the predecessor's head        *)
(* (`migrate_to_hybrid_if_due` steps 4-7: drain, lock, read `head_epoch_in`,*)
(* claim hybrid KeyPackages, stage the successor's opening commit).  It     *)
(* stages nothing on the DS; all it does is fix the value this migration    *)
(* will later claim to close.                                               *)
(*                                                                          *)
(* Requires the migrator to have DRAINED its own lineage first              *)
(* (`localEpoch = HeadEpochIn`): under `max_past_epochs = 0` a message must *)
(* be decrypted while its group is still at its epoch, so a migrator that   *)
(* moved across with a backlog outstanding would destroy the only key       *)
(* material that could read it.                                             *)
(*                                                                          *)
(* Splitting the migration into prepare/commit is what puts the race in the *)
(* model: every interleaving of another client's Submit BETWEEN these two   *)
(* steps is explored.                                                       *)
(***************************************************************************)
MigratePrepare(k, c) ==
    /\ member[k][c]
    /\ pending[k][c] = NoPend
    /\ localGen[k][c] = HeadGen(k)
    /\ HeadGen(k) < MaxGen                            \* bound the lineage count
    /\ localEpoch[k][c] = HeadEpochIn(k, HeadGen(k))  \* drained before migrating
    /\ pending' = [pending EXCEPT ![k][c] =
                       [gen |-> HeadGen(k), epoch |-> HeadEpochIn(k, HeadGen(k))]]
    /\ UNCHANGED <<log, localGen, localEpoch, member, adopted, seqCtr>>

(***************************************************************************)
(* MigrateCommit(k, c) -- publish the successor's opening commit, the       *)
(* SECOND accepting branch of the DS's `accepts()`: generation N+1 at epoch *)
(* 0, admitted only when the submitter also names the head it observed on   *)
(* generation N, and that is STILL generation N's head.                     *)
(*                                                                          *)
(* That naming is a compare-and-swap on the OLD lineage, and it is the      *)
(* entire safety content of P4.  Without it, a commit that lands in         *)
(* generation N between the migrator's read and its write is silently       *)
(* ORPHANED: it sits in a lineage every device is about to abandon, its     *)
(* messages unreadable by anyone who has moved on, and nothing in the       *)
(* system ever notices.  With it, the migration simply loses the race and   *)
(* retries.                                                                 *)
(*                                                                          *)
(* TEETH: SoundMigrate=FALSE drops the compare-and-swap (open on the stale  *)
(* observed head), producing exactly that orphan -- an                      *)
(* OpeningClosesTheHead violation.                                          *)
(***************************************************************************)
MigrateCommit(k, c) ==
    /\ member[k][c]
    /\ pending[k][c] # NoPend
    /\ Len(log[k]) < MaxCommits
    /\ LET p == pending[k][c] IN
        /\ p.gen = HeadGen(k)   \* another migrator did not already open the next
        /\ (SoundMigrate => p.epoch = HeadEpochIn(k, p.gen))   \* THE compare-and-swap
        /\ LET nc == [gen |-> p.gen + 1, epoch |-> 0, seq |-> seqCtr + 1,
                      author |-> c, closes |-> p.epoch] IN
            /\ log' = [log EXCEPT ![k] = Append(@, nc)]
            /\ localGen' = [localGen EXCEPT ![k][c] = p.gen + 1]
            /\ localEpoch' = [localEpoch EXCEPT ![k][c] = 1]
            /\ adopted' = [adopted EXCEPT ![k][c][p.gen + 1][0] = nc.seq]
    /\ pending' = [pending EXCEPT ![k][c] = NoPend]
    /\ seqCtr' = seqCtr + 1
    /\ UNCHANGED <<member>>

(***************************************************************************)
(* MigrateAbandon(k, c) -- roll the staged successor back (`abandon_        *)
(* successor`).  A migrator whose compare-and-swap loses simply deletes the *)
(* successor group and stays where it was; because the two lineages coexist *)
(* locally under distinct GroupIds, its own classic group is untouched.     *)
(* Modelled as always available so a lost race is never a deadlock.         *)
(***************************************************************************)
MigrateAbandon(k, c) ==
    /\ pending[k][c] # NoPend
    /\ pending' = [pending EXCEPT ![k][c] = NoPend]
    /\ UNCHANGED <<log, localGen, localEpoch, member, adopted, seqCtr>>

(***************************************************************************)
(* AdoptGeneration(k, c) -- a member moved across by Welcome                *)
(* (`apply_welcome` then `maybe_advance_generation`).  Hops exactly ONE     *)
(* generation, and only after DRAINING the current one, mirroring the hop   *)
(* loop in `process_one_generation`: adoption deletes the predecessor's key *)
(* material, and under `max_past_epochs = 0` that material is the only      *)
(* thing that can decrypt the pre-migration backlog.                        *)
(***************************************************************************)
AdoptGeneration(k, c) ==
    /\ member[k][c]
    /\ localGen[k][c] < HeadGen(k)
    /\ localEpoch[k][c] = HeadEpochIn(k, localGen[k][c])   \* drained first
    /\ localGen' = [localGen EXCEPT ![k][c] = localGen[k][c] + 1]
    /\ localEpoch' = [localEpoch EXCEPT ![k][c] = 0]
    /\ UNCHANGED <<log, member, adopted, seqCtr, pending>>

(***************************************************************************)
(* Apply(k, c) -- the client replay step                                    *)
(* (`process_pending_commits_locked_impl`, gated by the Kani-proved         *)
(* `invariants::classify`): if the NEXT commit (epoch = localEpoch, within  *)
(* the client's own lineage) is present, apply it -- adopt it and advance   *)
(* the local epoch by one.  The `classify` decision is exactly "Apply iff   *)
(* this row's epoch == current epoch": there is never an Apply across a     *)
(* gap.  In this abstract log the chain is gapless by construction, so the  *)
(* next commit is present whenever the client is behind; the gap-recovery   *)
(* branch is subsumed by ExternalJoin.                                      *)
(***************************************************************************)
Apply(k, c) ==
    /\ LET g == localGen[k][c]
           e == localEpoch[k][c] IN
        /\ e < MaxEpoch
        /\ HasEpoch(k, g, e)        \* classify => Apply: the exact next epoch exists
        /\ localEpoch' = [localEpoch EXCEPT ![k][c] = e + 1]
        /\ adopted' = [adopted EXCEPT ![k][c][g][e] = CommitAt(k, g, e).seq]
    /\ UNCHANGED <<log, localGen, member, seqCtr, pending>>

(***************************************************************************)
(* ExternalJoin(k, c) -- the recovery jump                                  *)
(* (`external_join_group` reached from the gap branch of                    *)
(* `process_pending_commits_locked_impl`): a behind client abandons         *)
(* step-wise replay and jumps its local epoch straight to its lineage's     *)
(* head.  GUARDED by member[k][c] (I5): a removed / revoked device can NOT  *)
(* rejoin the tree this way (fuzzer-finding-#2 leak).  The jumped-over      *)
(* epochs are NOT adopted -- their commits (and any messages sealed at      *)
(* them) are an accepted loss (loss (a): history before you (re)joined),    *)
(* which is exactly why NoForeignAdopt stays true: the client records no    *)
(* commit it did not apply.                                                 *)
(***************************************************************************)
ExternalJoin(k, c) ==
    /\ member[k][c]                        \* I5 gate
    /\ localEpoch[k][c] < HeadEpochIn(k, localGen[k][c])   \* behind -> recover
    /\ localEpoch' = [localEpoch EXCEPT ![k][c] = HeadEpochIn(k, localGen[k][c])]
    /\ UNCHANGED <<log, localGen, member, adopted, seqCtr, pending>>

(***************************************************************************)
(* Remove(k, c) -- eviction: c stops being a current member.  Monotone      *)
(* (TRUE -> FALSE).  Models the F5/I5 boundary so the ExternalJoin gate is  *)
(* exercised: after a Remove, c can never take the recovery path back in.   *)
(* Any migration it had staged is dropped with it.                          *)
(***************************************************************************)
Remove(k, c) ==
    /\ member[k][c]
    /\ member' = [member EXCEPT ![k][c] = FALSE]
    /\ pending' = [pending EXCEPT ![k][c] = NoPend]
    /\ UNCHANGED <<log, localGen, localEpoch, adopted, seqCtr>>

Next ==
    \E k \in Keys :
        \E c \in Clients :
            \/ Submit(k, c)
            \/ Apply(k, c)
            \/ ExternalJoin(k, c)
            \/ Remove(k, c)
            \/ MigratePrepare(k, c)
            \/ MigrateCommit(k, c)
            \/ MigrateAbandon(k, c)
            \/ AdoptGeneration(k, c)

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(***************************************************************************)
(* INVARIANTS                                                              *)
(***************************************************************************)

TypeOK ==
    /\ seqCtr \in 0..MaxCommits
    /\ member \in [Keys -> [Clients -> BOOLEAN]]
    /\ localGen \in [Keys -> [Clients -> Gens]]
    /\ localEpoch \in [Keys -> [Clients -> 0..MaxEpoch]]
    /\ adopted \in [Keys -> [Clients -> [Gens -> [Epochs -> 0..MaxCommits]]]]
    /\ \A k \in Keys : \A c \in Clients :
        \/ pending[k][c] = NoPend
        \/ /\ pending[k][c].gen \in Gens
           /\ pending[k][c].epoch \in 0..MaxEpoch
    /\ \A k \in Keys :
        /\ Len(log[k]) <= MaxCommits
        /\ \A i \in DOMAIN log[k] :
            /\ log[k][i].gen \in Gens
            /\ log[k][i].epoch \in Epochs
            /\ log[k][i].seq \in 1..MaxCommits
            /\ log[k][i].author \in Clients
            /\ log[k][i].closes \in {NoClose} \cup (0..MaxEpoch)

\* I1(a) -- one commit per (generation, epoch): no two distinct log entries
\* share the widened key. The core anti-fork property the DS conditional-insert
\* enforces, now scoped to a lineage so a successor's epoch 0 is not mistaken
\* for a duplicate of its predecessor's.
OnePerEpoch ==
    \A k \in Keys :
        \A i, j \in DOMAIN log[k] :
            (/\ log[k][i].gen = log[k][j].gen
             /\ log[k][i].epoch = log[k][j].epoch) => (i = j)

\* I1(b) -- gapless WITHIN each lineage: the epochs present in generation g are
\* exactly 0 .. HeadEpochIn(g)-1, no hole. With OnePerEpoch this means every
\* epoch below a lineage's head appears in it exactly once -- and, for a
\* generation that has any commit at all, that its FIRST commit is at epoch 0.
\* That last clause is the model-side twin of the transparency verifier's rule
\* (c) (`CommitLogInvariant`, verifiable-log-builder): a lineage may only ever
\* BEGIN, and it begins at its natural start, so a fork cannot be laundered as
\* a migration by bumping the generation and picking an arbitrary epoch.
Gapless ==
    \A k \in Keys : \A g \in Gens :
        { log[k][i].epoch : i \in IdxIn(k, g) } = 0..(HeadEpochIn(k, g) - 1)

\* I1(c) -- the conversation's monotone key never decreases, compared
\* LEXICOGRAPHICALLY on (generation, epoch). This is the widened form of the
\* old scalar HeadMonotone: at a migration the head epoch legitimately drops to
\* 0, and only the lexicographic reading makes that an advance rather than a
\* regression. Action property (robust under stuttering), like Spec B's
\* CursorMonotone.
LineageMonotone ==
    [][ \A k \in Keys :
          \/ Lineage'[k][1] > Lineage[k][1]
          \/ /\ Lineage'[k][1] = Lineage[k][1]
             /\ Lineage'[k][2] >= Lineage[k][2] ]_vars

\* P4's central safety property: OPENING A LINEAGE CLOSES THE PREVIOUS ONE.
\*
\* Every opening commit declares the predecessor head it observed (`closes`).
\* This asserts that declaration was TRUE and STAYED true: the predecessor
\* lineage contains no commit at or beyond it. Equivalently -- no commit is ever
\* orphaned into a lineage that has just been retired.
\*
\* This is what the compare-and-swap in `accepts()` buys, and it is exactly what
\* a plain "generation must increase" rule would NOT give you: two lineages can
\* be individually well-formed and still have lost a commit in the seam between
\* them. SoundMigrate=FALSE refutes it in a handful of steps.
OpeningClosesTheHead ==
    \A k \in Keys :
        \A i \in DOMAIN log[k] :
            (log[k][i].gen > 0 /\ log[k][i].epoch = 0) =>
                \A j \in IdxIn(k, log[k][i].gen - 1) :
                    log[k][j].epoch < log[k][i].closes

\* I2 -- no foreign adopt: every commit a client has adopted at (g, e)
\* byte-equals the log's commit there (abstracted as `seq` equality, seq being
\* the unique per-commit nonce). A client never installs a commit that is not
\* the one on the canonical log -- no phantom epoch, no fork adopted, and no
\* commit from a lineage it was never in.
NoForeignAdopt ==
    \A k \in Keys : \A c \in Clients : \A g \in Gens : \A e \in Epochs :
        (adopted[k][c][g][e] # NoSeq) =>
            \E i \in IdxIn(k, g) :
                /\ log[k][i].epoch = e
                /\ log[k][i].seq = adopted[k][c][g][e]

============================================================================
