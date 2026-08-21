# Proposal: `recur-progress-commitment` — cross-step loop state a window can verify instead of inherit

Status: proposed 2026-08-07, revised 2026-08-13 (revision 2)

Extracted from [`lazy-list-recur.md`](./lazy-list-recur.md) §5, which needs this and cannot
state it: §5 defines the per-iteration *facts* and the completeness *rules*, and assumes a
carrier for the accumulation those rules read. There is no sound carrier today.

Related:
- [`lazy-list-recur.md`](./lazy-list-recur.md) — **the consumer.** Its rules 1–8 and S1–S5
  accumulate across iterations; without this, they bind only in a window that happens to
  contain iteration 0, and the prover chooses the window. Ship together (§Dependency).
- [`paged-bytes.md`](./paged-bytes.md) — depends transitively: its sweep-coverage claim rests
  on `lazy-list-recur` §5.
- [`loop-carried-state.md`](./loop-carried-state.md) — proposes `TrackedStateRoot` "mirroring
  `active_drafts`" (`:280`), inheriting the identical gap. §7 explains why one mechanism should
  serve both.
- [`chain-fraud-proof.md`](./chain-fraud-proof.md) — the window model this extends. It already
  closed the neighbouring hole where the *window fingerprint* was host-claimed; the state a
  window opens with was not in its scope.
- [`trace-event-vocabulary.md`](./trace-event-vocabulary.md) — **revision 2 amends it.** The site
  lifetime events below follow its naming rule and its variant-order constraint.
- [`carried-state-channel.md`](./carried-state-channel.md) — **good to implement later, not
  now.** It generalizes this mechanism into one channel serving draft roots and loop-carried
  state roots as well. Deliberately deferred: a one-member category is speculative generality,
  and deferring costs nothing because adding a component breaks the trace format exactly as
  much as adding a field does (§7, §Uncertainty 1).
- [`recur-sequence-break.md`](./recur-sequence-break.md) — **depends on this.** It gives recur
  sequences an early exit, reusing §3.1's trace control bit and rewriting §6's `close_site`
  rule. It answers §Uncertainty 6; nothing here waits on it.

**Revision 2 changes — the design was not implementable as written.**

Revision 1 was attempted and backed out. Its §Modules touched says the recorder should stamp
`recur_progress_commitment`, "drive[n] from the existing `RecurExecutionState`". It cannot: that
struct holds `site_id`, `sequence_coordinates`, `site_coordinates`, `intra_sequence_index` and
`next_iteration_index`, and three of `RecurProgressFrame`'s fields have no source there. The
recorder replays `TraceEvent`s and never sees a `TileReplayJournal` — journals are produced later,
during replay in `raster-prover`, keyed by the very `StepRecord` that must already carry the
commitment, because the fingerprint is computed over step records.

The consequence is a **completeness** failure, not a soundness one, and it is total: the guest
would compute a real stack hash while the recorder could only write a placeholder, so *every*
recur program fails its own audit. Correct executions rejected, which is the opposite of the
failure mode the mechanism guards against.

The root cause is general and worth stating once, because it is the property every later field
must be checked against:

> **Commit-and-recompute requires input parity.** The producer that writes a commitment and the
> verifier that recomputes it must see the same facts. A verifier that sees *less* breaks
> soundness; a verifier that needs *more* breaks completeness. Revision 1 defined the frame over
> data that exists only on the verifier's side of the boundary.

Revision 2 fixes each offending field by its own cause — they are three different problems, and
one blanket remedy would have been wrong for two of them:

1. **`consumed_total` is deleted from the frame** (§1). It was never independent: rule 4 *defines*
   it. Checking the journal's value against the derived one, rather than folding it in, removes
   the asymmetry at no cost.
2. **`last_control` gets one bit on the iteration's trace event** (§3.1). This is the irreducible
   part — revision 1 already identified it as the only field the following step cannot derive —
   so the bit must cross the boundary, with the guest binding the host copy to the journal's.
3. **`source_len` was an ordering problem, not a visibility one** (§3.2), and revision 1 did not
   see it at all. `L` is authenticated at the site step, but a recur site's own event is emitted
   **after** every iteration it contains. No site-open event exists for either family, so no
   frame carrying an authenticated `L` can be pushed before iteration 0 — for the producer or the
   verifier. Revision 2 adds the site lifetime events, replacing the single after-the-loop event
   with a Start/End pair.

## Problem

The transition guest proves **one step per execution** (`guests/transition/src/main.rs:29`).
Anything that must hold *across* steps travels in `Transition`, which is pinned by the previous
journal's recursive receipt (`fraud_proof.rs:288`, `:303`) — sound for every `Next` step. A
window's **first** step has no previous journal, so its starting state comes from
`InitTransition`, read straight off the host.

That is fine for the fields that are re-derived or compared against something. It is not fine
for a map that is only ever read.

### What `Init` actually binds

| field | bound by |
| --- | --- |
| `init_frontier.position` | `assert_window_is_commitment_slice` derives the covering block range from it (`fraud_proof.rs:230`) |
| `init_frontier.leaf` / `ommers` | indirectly: they feed `frontier_root` for every window item, and items 1…w−1 are compared against the committed fingerprint |
| `init_storage_root` | the first step's `storage.root_before` assertion (`checks/store.rs:154`), and that record is fingerprint-pinned |
| `active_drafts` | **nothing** |
| `next_expected_coordinates` (absent at `Init`) | **nothing** — the window's first step's coordinates are unconstrained |

### The attack

Give `lazy-list-recur` §5 the obvious carrier — a per-site map in `Transition`, seeded from
`InitTransition` — and a fresh window opening at a recur site's own step can claim:

```text
site [2]:  next_iteration_index = 9,  consumed_total = 9
```

Rules 5 and 7 then pass over nine iterations that were never verified by anybody. The prover
picks where windows open, so this is not a corner case; it is the default way to defeat the
rules.

### The pattern the codebase already follows

`checks/drafts.rs` is the precedent, read correctly. The **anchor** for each draft step is
`DraftReplayTransition.root_before` — replay-proven, in the journal, and additionally
authenticated by a witness whose root must match it (`:55`). The carried `active_drafts` map is
*only* a continuity check, and it silently no-ops when the draft is absent (`:69`,
`if let Some(tracked_state) = ...`). So:

> **per-step anchor: authenticated. Carried map: continuity only, never the authority.**

A recur-progress map in `Transition` with no per-step anchor inverts that. This proposal
supplies the missing anchor.

### Two fixes that do not work

**Refuse to open mid-loop.** The guest can detect a mid-loop open from the first step's
coordinates plus the CFS, and panic. Sound, and unacceptable: refuting a divergence at
iteration 900 of a 1000-iteration loop forces the window to open at iteration 0, turning a
128-item window into a 900+-item one. Window size is the proving budget; a rule that makes it
data-dependent is not a rule anyone can operate.

**Prove one step of prefix.** `SerializableFrontier` carries `leaf` (`transition.rs:27`), which
*is* `hash_trace_item(step_{s−1})`, so the predecessor record can be proven for free. But rules
5 and 6 need that step's *journal* too (its `control`), which costs an extra `env::verify` per
window — and one step of carry is all it ever gives. It is a boundary patch, not a mechanism.

## Design

### 1. The state

```rust
// raster-core/src/recur_progress.rs — no_std, shared by the recorder and the guest
pub struct RecurProgressFrame {
    pub site: CfsCoordinates,
    pub kind: RecurSiteKind,             // Tile | Sequence
    pub chunk: u64,                      // C — the CFS literal, 1 when unchunked
    pub source_len: u64,                 // L — authenticated at the site Start event (§3.2)
    pub next_iteration_index: u64,       // the name the recorder already uses
    pub last_control: RecurControlKind,  // carried on the trace event (§3.1)
}

/// Innermost last. Nesting is strictly LIFO — the recorder models the active
/// site as a single `Option` (`recorder.rs:722`) and refuses an ordinary tile
/// while iterations are live (`:650`) — so this is a stack, not a map.
pub struct RecurProgressStack(Vec<RecurProgressFrame>);
```

Every field is under the commitment, each blocks a specific forgery, and — new in revision 2 —
each is reachable by the **recorder** as well as the guest. The last column is the parity check
every future field has to pass:

| field | what it stops | producer sees it via |
| --- | --- | --- |
| `site`, `kind` | attaching one loop's progress to another's iterations; the two kinds have different rule sets | CFS + step coordinates |
| `chunk` | re-declaring `C` mid-loop, which would make rule 4's `min(C, L − consumed_total)` partly prover-chosen | CFS literal (`RecurTileItem::chunk`) |
| `source_len` | switching `L` mid-loop | the site `Start` event's metadata selection (§3.2) |
| `next_iteration_index` | rules 1, 2 — first index is 0, indices are contiguous | `RecurExecutionState`, already tracked |
| `last_control` | rule 6. The **only** field the following step cannot derive from its own authenticated data — a `Break` is invisible to the iteration after it | the control bit on the iteration event (§3.1) |

**`consumed_total` is gone, and its absence is the point.** Revision 1 carried it as "derivable
but kept explicit so `close_site` compares rather than re-derives" — a readability call, per its
own Uncertainty 2. That convenience is what broke parity: it is the running sum of the journals'
`consumed_elements`, and the recorder has no journals.

It was never independent. Rule 4 *defines* it:

```text
consumed_elements == min(C, L − covered_before)
```

so once rule 4 is enforced, the honest total is fully determined by `(C, L,
next_iteration_index)` — all three of which the producer has. The correct relationship is
therefore the other way round from revision 1: the journal's `consumed_elements` is **checked
against** the derived value, never **folded into** the commitment. `close_site` re-derives
`min(next_iteration_index · C, L)` and compares.

This also sharpens what the journal is for. `lazy-list-recur` §5 already says the journal field
is "a binding, not an authority"; a value that is folded into a commitment is being treated as an
authority. Checking it instead is what that sentence actually implies.

**The learning direction is metadata first, and the empty sweep is why.** An earlier draft had
`source_len` learned at iteration 0 from the item proof and merely *cross-checked* against §1's
metadata. That inverts the motivating case: `lazy-list-recur` rule 7 — zero iterations valid iff
`L == 0` — is the forged-`len = 0` sweep, and an empty sweep has no iteration 0, hence no item
proof to learn from. Metadata at the site step is the only source that exists there, and
§Uncertainty 4 already treats it as the authority the terminal rules read. The per-iteration
re-assertion is not redundant with it: it is what re-anchors `L` inside a window that does not
contain the site step, where the frame arrives as a seed.

### 2. The commitment

```text
recur_progress_commitment = H(b"recur-progress" ‖ postcard(RecurProgressStack))
```

The empty stack has a canonical value, so *"no loop in flight"* is a positive statement in the
trace rather than an absent field. That is what lets **any** step seed a window, including an
ordinary tile inside a recur-sequence iteration.

### 3. Where it travels

| carrier | contents | pinned by |
| --- | --- | --- |
| `StepRecord.recur_progress_commitment: Hash32` | the commitment, **after** this step | fingerprint agreement with `commit.bin`, exactly like `storage.root_before` |
| `Transition.recur_progress: RecurProgressStack` | the preimage | `verify_previous_journal` + `assert_state_continuity` |
| `TransitionInput.window_start_recur_progress: Option<RecurProgressStack>` | the preimage, at a fresh `Init` | checked against the record — see §4 |

The preimage travels in the clear because the guest must **advance** it (`next_iteration_index
+ 1`, `last_control = …`) and a hash cannot be advanced. The commitment exists for the one thing
the preimage cannot do: seed a chain that starts mid-loop.

`InitTransition` gains **nothing**. That is the point of the proposal.

The two subsections below are what revision 2 adds, and both are **trace-format** changes on top
of the `StepRecord` field above. Fingerprints move for all three together; there is no ordering
in which they move separately, so they land as one migration.

#### 3.1 The control bit reaches the producer

`last_control` is the one fact that genuinely has to cross the boundary. The iteration's
`FnCallRecord` gains it:

```rust
// raster-core/src/trace.rs
pub struct FnCallRecord {
    pub fn_name: String,
    pub input: Option<FnInput>,
    pub output: Option<FnOutput>,
    pub draft_transition_witness: Option<DraftTransitionWitness>,
    /// `Some` on a recur-tile iteration, `None` everywhere else. Host-recorded,
    /// and bound to the replay-proven copy by the guest — see below.
    pub recur_control: Option<RecurControlKind>,
}
```

The recur-tile wrapper already knows the discriminant statically: `gen_replay_transition_binding`
matches `Continue | Break` for the journal, and the same `ProtocolReturnKind` match fills this.
Modes whose return type carries no `RecurControl` emit a literal `Continue`, exactly as the
journal does — never an absence, so no consumer applies a default.

**The guest must bind the two copies, and that assert is the whole safety of the arrangement:**

```text
trace_event.recur_control == replay_journal.recur.control
```

Without it the host copy is unconstrained, the commitment is computed over a number the prover
chose, and the mechanism proves nothing. The journal remains the authority; the trace copy exists
solely so the producer can compute the same commitment. Duplicating a fact is acceptable **only**
where an equality check makes the duplicate non-load-bearing.

#### 3.2 Site lifetime events, so `L` is authenticated before iteration 0

Revision 1 says `source_len` is "learned at the site step". It cannot be, and this is independent
of who sees what: `trace-event-vocabulary.md` records that a recur site's own event is emitted
**after** every iteration it contains (`raster-core/src/trace.rs`, the vocabulary table calls this
out as one of the two rows that surprise every reader). There is no site-open event for either
family — recur sequences have per-*iteration* `RecurSequenceIterationStart`/`End`, and the site
itself has only the trailing `RecurTileExec` / `RecurSequenceExec`.

So the frame cannot be pushed with an authenticated `L` before the iterations that are checked
against it. Neither the recorder nor the guest can do it.

The fix is to give a recur site the same lifetime shape a sequence already has — a Start/End
pair at the site coordinate `[s]`, replacing the single trailing event:

| event | level | published by | at coordinates |
| --- | --- | --- | --- |
| `RecurTileStart` / `RecurTileEnd` | item | recur-tile driver, **around the loop** | `[s]` |
| `RecurSequenceStart` / `RecurSequenceEnd` | item | recur-sequence driver, **around the loop** | `[s]` |

The names follow the vocabulary's own rule — `Iteration` in a name means the iteration level,
unmarked means the item — so `RecurSequenceStart` now denotes the site, which is what a reader
expects it to mean. That rule's doc comment cites the old `RecurSequenceStart` (which denoted an
iteration) as the confusion it was written to prevent; this reuses the freed name for the thing it
reads as.

**`Start` commits to what is about to run; `End` to what came out.** That is the convention the
vocabulary already uses — `SequenceStart` carries `FnCallRecord.input`, `SequenceEnd` carries
`output` — and it is the reason `L` belongs on `Start` by *role* rather than by byte-count
arithmetic. The loop's source is its precondition, and the `0x0A` metadata selection is the
authenticated statement about it, so it goes where preconditions go.

**This splits an event that already had both halves; it does not add a payload.** The recur
wrapper builds the whole `FnInput` — the source's metadata binding under the name `"input"`, plus
state, output draft and extra args — **before** the loop, then holds it and publishes it
afterwards next to the output (`raster-macros/src/recur.rs`):

```rust
let __raster_input = Some(FnInput { … });   // built here, before the loop
let result = #run_driver;                   // ← the loop
let __raster_output = Some(FnOutput::new(…));
publish(RecurTileExec(FnCallRecord { input: __raster_input, output: __raster_output, … }));
```

One event carries both halves only *because* it is a single `*Exec`. `Start` publishes the input
half at the point it was already computed; `End` keeps the output half and is where `close_site`
runs. The trailing event stops being an input carrier at all.

That is also why the missing site-open was easy to overlook for as long as it was: the input trace
was assembled at the right moment all along, and only its *publication* was late. Nothing reads as
wrong until something has to verify against it **before** the iterations run — which is exactly
what pushing a frame with an authenticated `L` requires.

**Variant indices are preserved, so this is not a trace migration on its own.** The vocabulary
warns that postcard encodes a variant by declaration index, so reordering is a migration while
*renaming* is free and *appending* is free. Both properties are available here:

```rust
pub enum TraceEvent {
    // ...unchanged through RecurTileIterationExec...
    RecurTileExec(FnCallRecord),        // renamed -> RecurTileEnd     (index unchanged)
    RecurSequenceExec(FnCallRecord),    // renamed -> RecurSequenceEnd (index unchanged)
    RecurTileStart(FnCallRecord),       // appended
    RecurSequenceStart(FnCallRecord),   // appended
}
```

The trailing events become the `End` half by rename, and the two `Start` variants are appended.
No existing index moves. The trace format still breaks — `FnCallRecord` gains `recur_control`, and
`StepRecord` gains the commitment — but the *event enum* contributes nothing to that break, which
keeps the diff reviewable and leaves any decoder-versioning question to the two field additions.

**The CFS gains nothing.** A recur site is already one item at `[s]`; `Start`/`End` are two events
at that coordinate, the way `SequenceStart`/`SequenceEnd` are. `record_matches_item` gains the two
new kinds against `RecurTile` / `RecurSequence` items, and
`try_get_recur_iteration_coordinates` is unaffected.

#### 3.2.1 Ordering a site open — resolved, and it was a two-line asymmetry

An earlier draft of this subsection claimed the expected-coordinate chain could not accept a site
`Start` and listed three options, recommending a signature change to
`CfsCursor::try_get_next_coordinates`. **That was wrong.** The claim, and the options, are
superseded by what follows. Implemented and verified 2026-08-13.

**What was actually wrong.** `CfsCursor::get_sequence` returns
`(&SequenceDef, Option<CfsCoordinate>)`, and `try_get_next_coordinates` branches on that `Option`:
`Some(i)` means "this coordinate is a leaf item, its successor is the next item"; `None` means
"this coordinate is a *scope*", and the `None` arm already builds the set

```text
{ [s]  (the coordinate itself),  [s][0]  (first child),  [s+1]  (next sibling) }
```

which is exactly what a site `Start` needs. A nested `Sequence` took the `None` path; a recur site
took the `Some` path and so looked like a leaf whose only successor was the next item. The two
differed by two lines in `get_sequence`, not by anything structural.

**The fix.** A bare recur-site coordinate `[s]` now yields `None` — a scope — for both
`RecurTile` and `RecurSequence`. Iteration coordinates `[s][i]` are unchanged. `Start` and `End`
then share `[s]` the way `SequenceStart`/`SequenceEnd` already do, and neither the CFS nor
`try_get_next_coordinates`'s signature moves.

**The widening is the existing convention, not a new hole.** With two records at `[s]` and a
successor relation keyed on the coordinate alone, the set must be the union of both halves'
successors — so after a site `End`, `[s][0]` remains ordering-legal. That is not introduced here:
a nested `Sequence` at `[0]` already yields `{[0], [0][0], [1]}`, measured directly rather than
reasoned about. What bounds the count is the progress rules, not the ordering check —
`close_site` has popped the frame, so a stray iteration after the site fails
`advance_*_iteration` with `NoActiveSite`. Ordering bounds the *shape*; the rules bound the
*count*, and duplicating that in the ordering check would be redundant.

**A second, pre-existing gap surfaced alongside it.** `try_get_next_coordinates` early-returns
`{site ++ [i+1], site}` for any coordinate that decomposes as a recur iteration — a set written
for the recur-*tile* shape, where an iteration is a single leaf `Exec`. A recur-*sequence*
iteration is a scope whose own steps live at `[s][i][j]`, so that set excluded the only step that
can legally follow it. The early return is now restricted to `RecurTile`, letting `RecurSequence`
fall through to the descend path. Covered by
`recur_sequence_iteration_offers_its_first_inner_step`.

This gap predates this proposal and is unrelated to the site events. It had gone unnoticed
because `call_recur_seq!` appears only in `crates/raster/tests/`, never in `examples/`, and
`hello-tiles` is the sole `--commit`/`--audit` fixture — so the guest's ordering chain had almost
certainly never run over a recur sequence. **A recur-sequence fixture taken through
`--commit`/`--audit` is still missing and should be added.**

**Verified.** Site `Start` events flow end to end, `hello-tiles` runs, and
`hello_tiles_audit_accepts_recur_trace_commitment` passes — the audit path being where the
ordering chain actually runs. `recur_site_completion_moves_to_next_sibling` asserted the old leaf
behaviour and became `recur_site_coordinate_offers_both_halves_and_the_next_sibling`.

### 4. The per-step check, and how a window opens

One rule, uniform for every step:

```text
advance(carried, this step's facts).commitment() == step.recur_progress_commitment
```

where `carried` is `Transition.recur_progress` for a `Next` step and
`window_start_recur_progress` for a fresh `Init`. The seed is therefore never believed: a
wrong seed advances to a different stack, hashes to a different value, and fails against the
recorded one. Only the true predecessor state survives, up to hash collision.

Recording only the *after* state (not a `before`/`after` pair) is what makes this work with one
32-byte field: the seed is validated by reproducing the step's own recorded commitment, not by
matching a predecessor record the window does not contain.

**What `advance` consumes, and what it merely checks.** Revision 2 separates these, because
conflating them is what broke parity:

| fact | role |
| --- | --- |
| `site`, `kind`, `chunk`, `source_len`, `iteration_index` | **advance the frame** — all producer-visible |
| `control` | **advances the frame** (`last_control`), via the trace bit of §3.1, bound to the journal |
| `consumed_elements` | **checked, never folded** — against `min(C, L − consumed_total)` (rule 4) |
| `ListRange.start` / `len`, or `Index(i)` + `List.len` | **checked** — proof-authenticated coverage cross-checked against the derived span (rule 8) |

Only the first two rows enter the commitment. Everything else is an equality assert against a
value the frame already determines, which is what lets the producer reach the same hash while the
verifier still rejects a journal that disagrees.

`advance` returns `Result<_, RecurProgressViolation>` in `raster-core`; the guest panics at the
call site. Same shape as `ChunkViolation` today.

One cheap extra assert: each frame's `site` must be a prefix of the step's coordinates, in
stack order, innermost matching. The commitment already pins the preimage, so this catches
recorder bugs rather than forgeries — but it makes a wrong-site stack fail loudly instead of
arithmetically.

### 5. Why the record and not the replay journal

A journal field would be strictly stronger *if the tile could see it*. It cannot: the tile
would have to receive the previous iteration's commitment through `RecurInput`, changing the
recur ABI, every recur tile's `input_commitment`, and putting an audit field in a user-facing
type — to carry a value the tile can only echo back. Nothing is gained, because the facts being
accumulated are **already** replay-proven; only the accumulation needs a carrier, and every
link of it is re-checked against those facts inside the window that contains the step.

### 6. What this is worth, precisely

The chain is verified link-by-link in every window, and at a window boundary the seed is
validated by reproducing the recorded commitment. What ultimately constrains that recorded
value is fingerprint agreement with `commit.bin`, at `bits_per_item` strength per item — with
the window's first item not itself compared (`finalize`'s `StepPosition::First`) but linked
forward into item 1, which is.

So recur progress ends up bound **exactly as strongly as `storage.root_before` and every other
per-step root**. The claim is not that it becomes unforgeable; it is that it stops sitting
outside the security argument and joins it. Today it would be reachable by `env::read()` and
compared against nothing at all.

### 7. The same gap, twice more

`active_drafts` (shipped) and `loop-carried-state`'s proposed `TrackedStateRoot` are the same
shape with the same `Init` hole. Drafts are partly protected — `root_before` is replay-proven,
so a forged map entry cannot invent a root out of nothing — but the *continuity* claim across a
window boundary rests on the same unchecked map, and the `if let Some(...)` means an absent
entry is not even a failure.

Folding all three under one channel is [`carried-state-channel.md`](./carried-state-channel.md),
and it should happen **after** this lands, not with it — see §Uncertainty 1 for why waiting is
free, and that proposal's §4 for the question drafts must answer first (`create_draft` emits no
trace event, so an absent entry is legitimate today).

## Dependency on `lazy-list-recur`

Strictly complementary, and neither is useful alone:

| | defines |
| --- | --- |
| `lazy-list-recur` §5 | the **facts** (`RecurTileReplay`, `RecurPosition`, `RecurControlKind`, replay-proven) and the **rules** (1–8, S1–S5) |
| this proposal | the **carrier** those rules accumulate in, and what makes it checkable when a window does not contain the whole loop |

Without §5 there are no authenticated facts to advance a frame with. Without this, §5's rules
constrain only a window that happens to contain iteration 0 — and the prover picks the window,
so in practice they constrain nothing. **They land in the same change**, as `lazy-list-recur`
phase 5. This proposal does not extend that proposal's scope: it replaces the
`InitTransition.active_recur_sweeps` field that phase would otherwise have added.

`paged-bytes` inherits the dependency through §5: "this program processed the whole artifact"
is exactly the claim these rules underwrite.

## Modules touched

| file | change | blast radius |
| --- | --- | --- |
| `raster-core/src/recur_progress.rs` | **landed** (2026-08-13): `RecurProgressFrame`, `RecurProgressStack`, `commitment()`, `advance_tile_iteration`, `advance_sequence_iteration`, `close_site`, `RecurProgressViolation`, 21 tests. Revision 2 removes `consumed_total` from the frame | additive |
| `raster-core/src/trace.rs` | `recur_progress_commitment: Hash32` on `StepRecord`; `recur_control` on `FnCallRecord` (§3.1); `RecurTileExec`/`RecurSequenceExec` renamed to `…End` and `RecurTileStart`/`RecurSequenceStart` appended (§3.2) | **trace encoding — fingerprints move**; event indices preserved |
| `raster-core/src/transition.rs` | `recur_progress` on `Transition`; `window_start_recur_progress` on `TransitionInput`. `InitTransition` unchanged | journal encoding — image ids move |
| `raster-core/src/cfs.rs` | a bare recur-site coordinate is a *scope*, so `Start`/`End` can share `[s]`; the recur-iteration early return restricted to `RecurTile` so a recur-sequence iteration can descend (§3.2.1) | small — **landed** |
| `raster-macros/src/recur.rs` | drivers emit the site `Start`/`End` pair around the loop; `Start` carries the source's metadata selection | small |
| `raster-macros/src/lib.rs` | recur-tile wrapper fills `recur_control` from the same `ProtocolReturnKind` match that fills the journal | small |
| `raster-runtime/src/tracing/recorder.rs` | push a frame at `…Start` (with `L` from the metadata selection), advance per iteration from `next_iteration_index` + the trace control bit, `close_site` at `…End`, stamp every step | **moderate — the highest-risk piece**; a mistake here silently yields commitments the guest rejects |
| `guests/transition/src/checks/cfs.rs` | the per-step advance/compare; the `trace.recur_control == journal.control` bind (§3.1); the completeness rules | additive |
| `guests/transition/src/fraud_proof.rs` | carry `recur_progress` through `LiveTransition` / `into_transition`; seed at genesis | small |
| `raster-prover/src/transition.rs`, `raster-cli/src/commands/run.rs` | pass the seed and the carried state through window assembly | small |
| `trace-event-vocabulary.md` | the vocabulary table gains the site lifetime rows; the "a recur site's own event is emitted **last**" note is retired | doc |

**Not** changed: `InitTransition`, the recur ABI, `RecurInput`, the replay journal's shape,
`.rindex`, the selector or proof-step enums, the CFS.

**Sequencing.** `lazy-list-recur` phases 1–4 and its §5 journal are **already implemented**; this
proposal is what remains of that proposal's phase 5. The three format changes here (step
commitment, `recur_control`, site events) move fingerprints together and must land as one
migration — there is no ordering in which they move separately.

## Verification

- **The attack, as a test:** a fresh `Init` at a recur site step with a seed claiming nine
  completed iterations is rejected, because advancing it does not reproduce the step's recorded
  commitment. This is the test the whole proposal exists for.
- A window opening at iteration 900 of a 1000-iteration loop verifies with an unchanged
  128-item window — the regression test for the rejected "refuse to open mid-loop" design.
- Recorder/guest agreement: the commitment the native recorder stamps equals the one the guest
  computes, over a fixture exercising element, chunked, and recur-sequence sites. A divergence
  between the two implementations is the failure mode a shared `raster-core` function exists to
  prevent, so it is asserted directly rather than implied.
- Every field is load-bearing, one test each: mutating `site`, `kind`, `chunk`, `source_len`,
  `next_iteration_index` or `last_control` in the seed must fail the compare. **Landed** as
  `every_frame_field_changes_the_commitment`; drop the `consumed_total` case with the field.
- **The completeness test, which is the one revision 1 lacked:** an honest end-to-end run of a
  program containing element, chunked and recur-sequence sites **verifies**. Revision 1's design
  fails this on every recur program, and would have failed it the first time anyone ran
  `--commit`/`--audit`. It belongs first in the list, not last: a verifier that rejects correct
  executions is a worse defect than one that is merely incomplete in its coverage, and no
  soundness test detects it.
- **Parity, asserted directly:** for each site kind, the commitment the recorder stamps equals
  the one the guest computes from the same trace. This is the property revision 1 violated, so it
  is checked as its own test rather than inferred from the end-to-end run passing.
- **The control bit is bound, not trusted:** a trace whose `recur_control` says `Continue` while
  the replay journal's says `Break` is rejected. Without this the host copy is prover-chosen and
  the commitment is computed over a number nobody proved — the single most important negative
  test in §3.1.
- **`consumed_elements` is checked, not folded:** a journal whose `consumed_elements` disagrees
  with `min(C, L − consumed_total)` is rejected even though the commitment matches. This is what
  demonstrates the two roles are actually separate; if the check were removed and the tests still
  passed, the field would have quietly become an authority again.
- **Site lifetime:** a site `Start` whose metadata selection authenticates `L`, followed by
  iterations checked against it — and the empty-sweep case, where `Start`/`End` bracket **zero**
  iterations and `L == 0` is still authenticated. The empty sweep is precisely the case that has
  no iteration 0 to learn `L` from, so it is the one that proves the ordering fix was necessary.
- A `Start` missing its metadata selection, or an `End` with no matching `Start`, is rejected.
- **A recur-sequence fixture through `--commit`/`--audit`.** `call_recur_seq!` appears only in
  `crates/raster/tests/`, never in `examples/`, and `hello-tiles` is the sole audit fixture — so
  the guest's expected-coordinate chain has almost certainly never run over a recur sequence.
  §3.2.1 found one pre-existing gap there by reading; a fixture is what would have found it by
  running, and is the cheapest insurance against the next one.
- Nesting: a `call_recur!` inside a recur-sequence iteration pushes and pops a second frame; a
  window opening inside the inner loop seeds both frames and verifies; a seed naming only the
  inner frame fails.
- The canonical empty stack: a window opening at an ordinary tile with `None` verifies against
  the empty-stack commitment, and a seed claiming a live frame where the record says empty
  fails.
- Coordinate consistency: a seed whose frames are not prefixes of the first step's coordinates
  is rejected.
- `Next` continuity is unaffected: an existing multi-step window carries the preimage forward
  with no seed supplied.

## Performance

+32 bytes per step record, +1 SHA-256 per step in the guest, +1 `postcard` encode of a small
struct. `StepRecord` already carries four 32-byte roots (`StorageRoots`), so this is
proportionate. No extra receipt verification, and — the whole point — no change to window size.

Revision 2 adds, on top of that:

- **+1 byte per recur-tile iteration** for `recur_control` (`Option<RecurControlKind>`, one byte
  when absent). One `RecurProgressFrame` also shrinks by 8 bytes with `consumed_total` removed.
- **+1 trace step per recur site**, the `Start` half. Its payload is **not** new bytes: the
  source's metadata selection and the site's other input bindings move off the trailing event
  onto `Start` (§3.2), so the wire cost is roughly the framing of one extra `FnCallRecord`, not
  the 41-byte selection itself. Per *site*, not per iteration — a 100 000-element sweep pays it
  once.

  What it does cost is one more step record per site, which marginally shifts where fixed-size
  fraud windows fall. Worth measuring on `hello-tiles`' four sites; not a structural cost.

The site `End` half is a rename of an event that already existed and keeps the output half, so it
adds nothing.

## Uncertainties for review

1. ~~Should the field be a general `carried_state_commitment` from the start?~~ **Resolved: no,
   keep the specific name.** The argument for wrapping was that it would save a trace-format
   break when drafts and loop-carried state join. It does not: the commitment is a hash over a
   struct, so adding a component changes the encoded bytes — even where that component is empty
   — hence every step's commitment, hence every fingerprint. Adding a component later costs
   exactly what adding a second `StepRecord` field costs. With the cost argument void, what
   remains is coordination, which a cross-reference handles: see
   [`carried-state-channel.md`](./carried-state-channel.md), to be done when a **second**
   component is actually ready. Naming a one-member category now would fix a shape for state
   whose requirements are not yet designed.
2. ~~Whether `consumed_total` stays.~~ **Resolved: removed** (§1, revision 2). It was recorded
   here as a readability call with no soundness weight, which was true — and missed that it had
   a *completeness* weight. Being the running sum of a journal field, it was the one frame entry
   the recorder could not reach, so keeping it made the whole mechanism unimplementable. The
   general lesson is worth carrying to the next field: "derivable, kept for readability" is not
   free when the two parties computing the value have different inputs.
3. **Whether the empty-stack commitment should be a compile-time constant** (as
   `EMPTY_LEAF` is in `merkle_tree.rs:27`) rather than computed. Cheap either way; a constant
   makes the "absence is a claim" property visible at the definition site.
4. **Whether `close_site` should also be replay-proven.** The site step carries no replay proof
   (`expected_tile_image_id` matches only `ExecTarget::Tile`), so the terminal rules read
   host-recorded structure plus the authenticated metadata length. That is the same standing as
   every other non-tile step, but it is the weakest link in the chain and should be stated in
   any release note rather than discovered later. Revision 2 does not change this: `End` is a
   host-emitted event like the trailing event it replaces.
5. ~~Whether `Start` should carry the metadata selection, or re-derive it.~~ **Resolved: it
   carries it, and the alternative was incoherent.** The question was posed as a wire-cost
   trade-off — 41 bytes per site against re-deriving from "the site's own input binding". But the
   site's input binding *is* the metadata selection, and it lives on the event that arrives too
   late; there was never a second option to weigh. `Start` carries the input half because
   `Start` is where inputs go (§3.2), and because the wrapper already computes it before the
   loop, so the split moves a payload rather than adding one.
6. ~~Whether recur *sequences* need the control bit at all.~~ **Resolved: keep it**, by
   [`recur-sequence-break.md`](./recur-sequence-break.md). The question assumed recur sequences
   cannot terminate early (`recur.rs:317`), which makes `last_control` constant `Continue` for a
   `Sequence` frame and tempts a `Tile`-only field. That proposal gives them a break — decided by
   a tile, since a sequence body cannot inspect values — so the field stops being constant and
   the one-shape frame is right for the ordinary reason rather than by default.

   Nothing here blocks on that proposal; this simply records why the field stays. Implement this
   revision as specified, and `recur-sequence-break` extends it afterwards.
