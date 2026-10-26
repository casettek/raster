# Proposal: `storage-prover` — prove the storage transition of a segment of steps in its own guest

Status: **Implemented 2026-10-21** (§Implementation record). Proposed the same day at `0474a76`.
Steps 1–4 landed together rather than as separate merges. Measured in dev mode on `hello-tiles`
(§Measured after implementation): the storage check turned out to be **about two thirds of the
transition guest's user cycles** on both `hello-tiles` and `prompt-prepare`, so moving it out
shortens the sequential transition chain by about half while the window's total stays level
(segments +0% and +7.7%). GPU timing is still to run.

Scope: **a segment of transition steps**, of any length from one step to the whole window, chosen
by the host. This is the shape [`control-flow-prover`](./control-flow-prover.md) ended with
(§Segments), adopted from the start rather than reached through a per-step-only guest.
§Differences from the control-flow prover lists where the two guests are not alike, and why.

Related:
- [`control-flow-prover`](./control-flow-prover.md): the template. Same segment, entry, journal,
  evidence and pinning pattern. This proposal reuses its Merkle tree and its host-side segment
  setting.
- [`transition-split`](./transition-split.md): its `store` part, and the two columns it says
  `store` owns (`storage_roots_before` and `selection`). This proposal turns those columns into
  entry fields (§What it commits). Like `control-flow-prover`, it extracts one family at the
  transition guest's current granularity and does not commit to the aggregator design.
- [`storage-write-cost`](./storage-write-cost.md), [`storage-role-split`](./storage-role-split.md):
  host-side storage cost. Not affected. This proposal moves guest work, not recorder work.
- [`dynamic-index-selection`](./dynamic-index-selection.md): `verify_bound_index_bindings`, which
  moves unchanged.

## Problem

`LiveTransition::apply_verified_step` (`guests/transition/src/fraud_proof.rs:687`) runs the storage
check as one call near the end of the step:

```
fraud_proof.rs:772  checks::store::verify_storage_transition(record, input_source_witness,
                        storage_selection_witnesses, output_witness, storage_witness,
                        &mut self.storage_frontier, &self.storage_index_root)
```

It checks four things (`checks/store.rs:139`):
1. **The selection witness shape**, on every step. A binding the step consumes carries its
   payload, and a binding it only forwards carries only the selected root. This is the guard
   against the 106 MB `List<MergeBucket>` (comment at the top of the function).
2. **Reads**, on steps with storage roots: each storage binding's coordinates commit to its
   commitment, through an append-log path and a 256-level coordinate-index membership proof
   (`verify_storage_read_witness`, `store.rs:49`). Then the selection witness folds to that
   commitment, by payload or by reference, and `verify_bound_index_bindings` runs over the whole
   map.
3. **The write**, if any: the entry is appended to the frontier, the index goes from
   non-membership to membership at the new log position, and the recorded `root_after` and
   `index_root_after` are the results.
4. **No write**: roots are unchanged, and a writing step kind that wrote nothing records no
   output commitment.

The function is close to self-contained already. It reads no CFS, no trace root and no replay
journal (`_output_witness_bytes` is an unused parameter). But it is tied to the rest of the step in
three places, and all three are hidden by the shared function:

1. **It owns state that other families read.** `advance_recur_progress` reads the current storage
   roots for a returned-state read (`fraud_proof.rs:737`), and `verify_program_end` reads them for
   the output read (`fraud_proof.rs:752`). Both rely on the frontier and index root being exactly
   what `checks::store` produced on earlier steps.
2. **Other families rely on it to verify the selection witnesses.** `recur_item_selection`
   (`checks/recur.rs:311`) hands out an item witness "already verified by `checks::store`", and
   `check_iteration_item` (`raster-core/src/recur_progress.rs:663`) reads the item's proof steps
   and list width from it. `authenticated_source_len` (`recur.rs:96–140`) decodes a site's `L`
   from the source witness's bytes on the same assumption.
   `io::verify_input_roots` (`checks/io.rs:141`) compares each witness's structural root with the
   replay's input roots, and relies on `checks::store` for the fold that ties that root to the
   stored object. So recur and io are sound only because store runs in the same execution.
3. **It relies on an anchor checked by another family.** Every read is keyed by
   `input_source_witness.storage()`. The check that this witness is the one the record commits to
   is `verify_input_source_anchored`, which `io::verify_step_record` calls. (The control-flow guest
   calls it too, but for its own copy.)

Because of these, the storage check cannot be tested, reviewed or proven apart from the rest of the
guest. Every transition step also decodes the full `StorageWitness`, which is up to three
256-sibling index proofs and a 32-level log path per binding plus the write, even though nothing
else reads it.

## Goal

1. Make the storage check a **pure function of one record, its storage witnesses, and the storage
   state before the step**, returning the state after. Do this inside today's guest first.
2. Move that function into a **storage guest** that proves a segment of consecutive steps. The
   transition guest verifies the receipt, finds its own entry, checks that the entry agrees with
   its own state, and adopts the entry's state after the step.

Non-goals:
- No change to what any storage rule accepts.
- No change to `StepRecord`, the trace, `commit.bin`, the fingerprint, `InitTransition` or the
  `Finished` journal's meaning. `TransitionJournal` gains `storage_image_id`, and `Transition`
  (the `Next` state) replaces its storage frontier with a smaller `StorageState` (§Step 4).
- The other storage reads stay where they are: the genesis entry-argument read
  (`checks::entrypoint::verify_genesis_authorization`), the program-output read
  (`verify_program_end`) and the returned-state read (`verify_returned_state`). They need only the
  roots, which the transition guest still holds, and each is tied to its own family's rule.
- No payload-free transition guest. That is §Deferred.

## What the storage check reads

After steps 1–3, this is the complete input for one step:

| input | source | how it is trusted |
| --- | --- | --- |
| `step_record` | host | `hash_trace_item(record)`, which the transition guest equates |
| `trace_position` | counted from the segment's `first_position` | transition guest equates it |
| `before: StorageState` (position, append-log root, index root) | the transition guest's storage state before the step | transition guest equates it |
| `frontier: SerializableFrontier` | host, **segment start only** | must fold to `before.root` at `before.position`. Later steps carry the guest's own frontier |
| `input_source_witness: Option<FnInput>` | host | **anchored**: its commitment equals `record.input_source_commitment` (checked in the storage guest) |
| `storage_selection_witnesses` | host | **anchored** through `input_source_witness.storage()[name].selection`, and committed (`selection_witnesses_commitment`) because the transition guest reads them too |
| `storage_witness: Option<StorageWitness>` | host | checked against `before` (reads) and against the record's roots (write) |

And the complete output: `after: StorageState`.

It does not read: the program or CFS, the trace root, input/output bytes, the replay journal,
draft state, recur progress or the authorization journal.

## Design

### Steps 1–3: inside today's transition guest

Each step keeps a single guest and can be merged on its own.

**Step 1: give the storage state a type, and return it.** Replace the `&mut NonEmptyFrontier` and
`&[u8]` pair with an owned state, and drop the unused `_output_witness_bytes`:

```rust
// raster-core/src/storage_check.rs (new; shared by host and both guests)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageState {
    /// The append-log frontier's position.
    pub position: u64,
    /// The append-log root, `frontier_root(frontier)`.
    #[serde(with = "serde_bytes")]
    pub root: Vec<u8>,
    /// The coordinate-index root.
    #[serde(with = "serde_bytes")]
    pub index_root: Vec<u8>,
}
```

`(position, root)` binds the frontier: the frontier's shape (which levels hold an ommer) is fixed
by its position, so two frontiers at one position that fold to the same root are a collision in
`combine_merkle_level`. This is what lets the transition guest stop holding the frontier in step 4,
and it is why `position` is there: the write check uses the new log position
(`verify_storage_write_witness`, the `log_position` assertion), so a frontier at the wrong position
would let a write claim the wrong slot.

**Step 2: make the input-source anchor part of the storage check.** `verify_storage_transition`
calls `guest_common::input_source::verify_input_source_anchored` first. The hash then runs twice
in the transition guest (here and in `io`) until step 4, the same interim cost `control-flow-prover`
step 3 accepted.

**Step 3: group behind one function with an explicit input and output.**

```rust
// guest_common/src/storage.rs (moved from transition/src/checks/store.rs)
pub struct StorageInput<'a> {
    pub step_record: &'a StepRecord,
    pub input_source_witness: Option<&'a FnInput>,
    pub storage_selection_witnesses: &'a BTreeMap<String, SelectionWitness>,
    pub storage_witness: Option<&'a StorageWitness>,
}

/// Check one step's storage transition from `frontier` (which must fold to
/// `before`) and advance `frontier` past its write. Returns the state after.
pub fn verify_step(
    frontier: &mut NonEmptyFrontier<Bytes>,
    before: &StorageState,
    input: &StorageInput<'_>,
) -> StorageState
```

The body is today's `verify_storage_transition`, unchanged except for the signature and step 2's
anchor. `storage_leaf_hash`, `append_log_root_from_witness` and `verify_storage_read_witness` move
with it into `guest_common::storage`, because recur and entrypoint in the transition guest keep
calling `verify_storage_read_witness`.

`apply_verified_step` builds the input from its own state and calls `verify_step`. After step 3
the guest behaves as it does today, and the storage check is a function that could run anywhere.

### Step 4: the storage guest

**What the guest reads.** A segment header, then **one frame per step**:

```rust
// raster-core/src/storage_check.rs
pub struct StorageSegmentHeader {
    /// The append-log frontier before the segment's first step.
    pub start_frontier: SerializableFrontier,
    /// The coordinate-index root before the segment's first step.
    pub start_index_root: Vec<u8>,
    /// The trace position of the segment's first step.
    pub first_position: u64,
    pub step_count: u64,
}

pub struct StorageStep {
    pub step_record: StepRecord,
    pub input_source_witness: Option<FnInput>,
    pub storage_selection_witnesses: BTreeMap<String, SelectionWitness>,
    pub storage_witness: Option<StorageWitness>,
}
```

The control-flow guest reads its whole segment in one frame. The storage guest must not: a step's
selection payloads and index proofs are large, and a window's worth in one heap is the
`List<MergeBucket>` failure again. Streaming keeps peak heap at one step, as in today's
transition guest.

```rust
// guests/storage/src/main.rs
fn main() {
    let header: StorageSegmentHeader = frame::read();
    let mut frontier = deserialize_frontier(&header.start_frontier).expect("valid start frontier");
    let mut state = StorageState {
        position: frontier.position().into(),
        root: frontier_root(&frontier),
        index_root: header.start_index_root,
    };
    let mut leaves = Vec::with_capacity(header.step_count as usize);
    let mut entries = Vec::with_capacity(header.step_count as usize);
    for offset in 0..header.step_count {
        let step: StorageStep = frame::read();
        let before = state.clone();
        state = storage::verify_step(&mut frontier, &before, &step.as_input());
        let entry = StorageStepEntry {
            step_record_hash: hash_trace_item(&step.step_record).to_vec(),
            input_source_commitment: step.input_source_witness.as_ref().map(input_source_commitment),
            selection_witnesses_commitment: selection_witnesses_commitment(&step.storage_selection_witnesses),
            trace_position: header.first_position + offset,
            before,
            after: state.clone(),
        };
        leaves.push(entry.leaf_hash());
        entries.push(entry);
    }   // `step` dropped here: its witnesses never outlive their step
    let journal = StorageJournal {
        first_position: header.first_position,
        entry_count: header.step_count,
        entries_root: entries_root(&leaves),
    };
    env::commit_slice(&journal.encode());
    env::write_slice(&postcard::to_allocvec(&entries).expect("entries always encode"));
}
```

**What it commits.**
- **One `StorageStepEntry` per step**, including steps without storage roots, so a step's slot is
  always `trace_position − first_position`. A rootless step still has checks to pass (the witness
  shape, and "only execution steps may carry storage witnesses"), and its entry has
  `before == after`.

  ```rust
  pub struct StorageStepEntry {
      pub step_record_hash: Vec<u8>,                     // hash_trace_item(step_record)
      pub input_source_commitment: Option<Vec<u8>>,      // as in ControlFlowEntry
      pub selection_witnesses_commitment: Vec<u8>,       // sha256(domain ‖ postcard(map))
      pub trace_position: u64,
      pub before: StorageState,
      pub after: StorageState,
  }
  ```

- **The journal commits only a Merkle root over them:** `StorageJournal { first_position,
  entry_count, entries_root }`, as raw postcard bytes. The tree is control-flow's
  (`entries_root`, `entry_path`, `root_from_path`), with its own leaf and node domains
  (`raster/storage/entry/v1`, `raster/storage/node/v1`). Step 4 moves those three functions out of
  `raster-core::control_flow` into a shared `raster-core::segment_tree` that takes the domain pair
  as a parameter, with control-flow's existing domains unchanged.
- **No `program_commitment`.** The storage check reads no program. Every entry is tied to the
  window by `step_record_hash` and `trace_position`, which the transition step equates.
- **The entries go to the host on stdout**, untrusted, as in control-flow: the host refuses them
  unless they fold to the committed root, and every transition step checks its own again.

`selection_witnesses_commitment` is the field with no counterpart in control-flow. See
§Soundness. `input_source_commitment` is redundant by design, as in control-flow.

**What each transition step checks.** It gets `StorageEvidence { journal, entry, path }`, built
by the same code as `ControlFlowEvidence`. Then:
1. `env::verify(storage_image_id, journal)`, unconditionally,
2. its trace position falls in the segment (`index_of`),
3. its entry folds along the path at that index to `entries_root`,
4. it equates the entry with its own facts:

```rust
// transition/src/checks/store.rs (replaces the moved check)
let storage_before = self.storage.clone();          // StorageState, not a frontier
let entry = checks::store::verify_storage(
    &params.storage_image_id,
    &input.storage,                                 // StorageEvidence
    &checks::store::StorageFacts {
        step_record: &input.step_record,
        input_source_witness: input.input_source_witness.as_ref(),
        storage_selection_witnesses: &input.storage_selection_witnesses,
        trace_position: self.frontier.position().into(),
        before: &storage_before,
    },
);
// recur and entrypoint read `storage_before.root` / `.index_root`, as today
// … other families unchanged …
self.storage = entry.after.clone();
```

The equality on `before` comes first in the step, ahead of recur and entrypoint, so their reads
run against roots the storage receipt has vouched for. Adopting `after` comes last.

`TransitionInput` loses `storage_witness`, which only the storage check read, and gains
`storage: StorageEvidence`. `input_source_witness` and `storage_selection_witnesses` stay, because
io and recur read them.

`Transition` (the `Next` state) replaces `storage_frontier` and `storage_root` with
`storage: StorageState`. The transition guest no longer appends, so it has no use for the
frontier, and a frontier is up to 32 ommers per `Next` journal. `InitTransition` keeps
`init_storage_frontier`: `assert_opens_at_genesis` reads it, and the window's first step derives
its `StorageState` from it (after checking `frontier_root` against `init_storage_root`, as today).

**Host.** Every segment's start can be computed before any proving: start from the window's
`init_storage_frontier` and index root and, for each step, take the recorded `root_after` and
`index_root_after`, appending `storage_leaf_hash(entry)` to the frontier for each step that
writes. The host checks that its frontier's root matches the recorded `root_after` at every
writing step and panics early if it doesn't. The guest repeats every check, so this is a debugging
aid, not a trust boundary.

```
crates/raster-core/src/segment_tree.rs          # entries_root, entry_path, root_from_path (moved, domain-parameterized)
crates/raster-core/src/storage_check.rs         # StorageState, StorageSegmentHeader, StorageStep,
                                                #   StorageStepEntry, StorageJournal, StorageEvidence
crates/raster-prover/guests/guest_common/src/
└── storage.rs                                  # verify_step + read/write witness checks, moved from transition
crates/raster-prover/guests/storage/            # new guest
├── Cargo.toml
└── src/
    ├── main.rs
    └── tests.rs                                # the storage tests from transition's tests/mod.rs, moved
crates/raster-prover/guests/transition/src/checks/
└── store.rs                                    # now only verify_storage: receipt + path + equalities
crates/raster-prover/src/storage.rs             # host: plan_storage_segments, ProvenStorage,
                                                #   storage_guest_image_id()
```

**The modes.** `--storage-segment window|<n>`, default `window`, `1` being per transition step.
The setting type is `ControlFlowSegments` renamed to `SegmentLength` and moved to
`raster_prover::segments`, so both flags parse and plan the same way. The flags stay separate
because the two guests' costs differ (§Cost). The guests, the journal, the transition step's check
and the image ids are the same in every mode, so a relying party cannot tell which mode produced a
proof, and doesn't need to.

### Image-id pinning

As control-flow's: `PublicParams::read` reads `storage_image_id` as one more frame,
`TransitionJournal` commits it, the transition guest asserts it is unchanged across `Next` (an
`assert_storage_continuity` next to the existing two), and `verify_fraud_proof_receipt` pins it
against `storage_guest_image_id()`. Both halves are required: `authorization-image-id-unpinned`
was a pin with no continuity check. `chain_fraud` is not involved, for the reason
`control-flow-prover` gives.

## Soundness

**Claim:** after step 4, the transition guest accepts a step exactly when the step-3 guest would
have accepted it.

In step 3, `storage::verify_step` runs on inputs from two places:
- **The guest's own state**: the record, the trace position, and the storage state before the
  step. In step 4 each is an entry field the transition step equates with its own value. The
  record is compared by `hash_trace_item`, the trace leaf's hash.
- **Host witnesses**: the start frontier, `input_source_witness`, the selection witnesses and
  `storage_witness`. Inside `verify_step` each is checked against the first group: the frontier
  against `before`, the input source against the record, the selections against the
  input-source bindings, and the storage witness against `before` and the record's roots.

Inside a segment, the guest derives every later step's `before` from the previous step's `after`
and carries its own frontier, so only the start is host-supplied, and every transition step
equates its own `before`. As in control-flow §Segments, each transition step does all of this
itself. A check one step made on the segment says nothing about another.

**The new equality is load-bearing, not redundant.** `input_source_commitment` repeats what the
record already anchors. `selection_witnesses_commitment` does not. The witnesses are anchored to
the record only through the folds `checks::store` runs, and recur and io read them assuming those
folds ran (§Problem, 2). Without this equality the transition guest could be handed a selection
map that differs from the one the storage guest verified. For example: a recur item witness whose
`bytes` claim a different list width, folding to nothing, read by `check_iteration_item` while the
storage receipt covers an honest map. The commitment is over the whole map, every entry, so no
consumer has to be listed for it to hold. This is the hazard `transition-split` calls a missing
column, closed the way it proposes.

**The state after is trusted as committed.** `after` is computed by the storage guest from checked
inputs and stored as-is, like control-flow's `expected_after`. The next step equates its `before`
with it, and the window's last step's state is never read again.

## Cost

*Written before implementation. §Measured after implementation has the numbers: the storage check
was not a control-flow-sized share but most of a step.* What changes per step:

| per step | removed from the transition guest | added |
| --- | --- | --- |
| index membership/non-membership folds (256 levels each), log-path folds, selection folds, bound-index check, frontier append | yes | moved to the storage guest |
| decoding `storage_witness` | yes | moved |
| decoding and re-serializing the storage frontier in `Transition` | yes | — (one decode per segment in the storage guest) |
| `env::verify` of the storage journal, path fold, equalities | — | yes |
| hashing the selection witness map | — | once in each guest |
| the storage guest's fixed work | — | once per segment: frame reads and the journal. **No program decode, no `CfsCursor::new`** |

Two things set it apart from the control-flow numbers:
- **The fixed cost per segment should be much smaller.** The control-flow guest spent 691–859k of
  its 2.1–2.6M cycles on setup, mostly the program decode and `CfsCursor::new`. The storage guest
  needs neither. Per-step mode should therefore cost less than control-flow's +22–25% total
  cycles, though every execution is still at least one risc0 segment.
- **The moved work is probably a larger share.** The CFS check was 1.1–1.7% of a step. The
  storage check folds 256-level index proofs per binding and hashes selection payloads, so it is
  likely several times that. That share is what leaves the sequential transition chain: storage
  receipts depend on no transition receipt, so with a second prover they come off the critical
  path. On one GPU the total only grows. The new cost is the map hash in the transition guest,
  which is proportional to payload bytes on payload-heavy steps.

Step 0 has to measure both before step 4 is built. If the storage check turns out to be a
control-flow-sized share, steps 1–3 are still worth landing, and step 4 is justified only by
isolation, as control-flow's step 6 was.

## Differences from the control-flow prover

| | control-flow prover | storage prover |
| --- | --- | --- |
| **State** | Nearly stateless. Carries `expected_coordinates` and derives trace facts by appending record hashes | Stateful. Carries the append-log frontier and index root, and **produces** the state the transition guest adopts and recur and entrypoint read |
| **Entry** | `expected_before` / `expected_after`, plus trace facts it derived | `before` / `after: StorageState`. The transition guest stops holding a frontier, and `Transition` shrinks |
| **Program** | Decodes `program.bin` and builds `CfsCursor`, so the journal carries `program_commitment` | Reads no program. No decode, no `program_commitment`, much less fixed work |
| **Trace root** | Needs `trace_root_before` (scope-parent fold), derived per step by a 32-level fold | Doesn't need it. Counts `trace_position` from `first_position` |
| **Witnesses shared with the transition guest** | `input_source_witness` only, and its equality is redundant | `input_source_witness` (redundant) **and the selection witness map, whose equality is load-bearing** (§Soundness) |
| **Witnesses that leave the transition guest** | scope witness, scope parent | `storage_witness` (log paths, index proofs, write proofs). Selection payloads stay (§Deferred) |
| **Input shape** | One frame per segment | **One frame per step**, because per-step witnesses are large |
| **Entanglement to undo first** | Chunking read the replay journal (moved out, step 2), plus the anchor | None in that direction: store reads nothing from other families. The coupling runs the other way: recur and io depend on store's verification, so the work is equalities, not code moves |
| **Host planning** | Trace frontier plus native `expected_successors` | Append-log frontier rebuilt from recorded `root_after` and write entries |
| **Expected payoff** | Isolation only (1–2% of a step) | Isolation, plus taking a probably larger share off the sequential chain. Unmeasured |

## Migration

*All of steps 0–4 are done (§Implementation record). Step 0 was answered by the step-4 build
itself rather than by a probe inside the monolith.*

0. **Measure.** Wrap `verify_storage_transition` in `env::cycle_count()` (temporary probe) and
   run the windows control-flow used: `hello-tiles --fraud-step 27` (32 steps) and
   `prompt-prepare --fraud-step 796` (16 steps), dev mode. Record the storage share of
   `apply_verified_step` and of the step, and the serialized size per step of `storage_witness`
   and of `storage_selection_witnesses`. The second size is what the new map hash costs.
1. **`StorageState`**, with `verify_storage_transition` returning it and dropping
   `_output_witness_bytes`.
2. **The anchor inside the storage check.**
3. **`StorageInput` and `storage::verify_step`**, moved with the read/write helpers into
   `guest_common::storage`. Split the storage tests out of `tests/mod.rs` by the function under
   test. Tests that drive storage together with recur or io stay in `mod.rs`, as the site-`Start`
   tests did for control-flow.
4. **Add the storage guest**, segments included. Move the tree into `raster-core::segment_tree`,
   rename `ControlFlowSegments` to `SegmentLength`, and add `--storage-segment`. Add
   `storage_image_id` to `PublicParams` and `TransitionJournal`, with the continuity assertion and
   the pin. Replace `Transition`'s frontier with `StorageState`. Tests:
   - one tamper test per entry field, where changing it by one byte makes the transition guest
     reject,
   - for `selection_witnesses_commitment`, a matching journal handed to a transition step with a
     different selection map (one recur item's `bytes` changed),
   - `one_segment_and_one_step_segments_commit_the_same_entries`, as in control-flow,
   - a segment whose start frontier has the right root at the wrong position is refused,
   - another step's entry from the same segment is refused.

   Then measure each mode on both windows, as control-flow §Segments did.

Each of steps 1–3 leaves one guest and a passing suite. Stopping after step 3 is a valid end
state. Every step changes the transition guest's code, so its image id moves each time. Only
step 4 changes the serialized `TransitionInput`, `Transition` and `TransitionJournal`.
`raster-core` gains code tiles don't call, so tile ELFs should not change: check with `cmp` on a
`hello-tiles` tile, not with `Raster.lock`, which is path-dependent in this checkout.

## Open questions

1. **Hash the map, or re-fold the selections in the transition guest?** The alternative to
   `selection_witnesses_commitment` is to have recur and io call `verify_selection_witness` on the
   bindings they read, against the anchored `storage_meta.selection`. That needs no new entry
   field, but it repeats structural folds instead of one flat hash, and it relies on every
   consumer remembering to fold. Recommended: the commitment. Step 0's byte sizes confirm or
   overturn this.
2. **Should the three other storage reads move?** The genesis, program-output and returned-state
   reads are storage reads by mechanism, but each answers its own family's question and needs
   only roots the transition guest holds, equated. Moving them would mean kind-specific witnesses
   in `StorageStep` and new "read verified" facts in the entry. Recommended: no.
3. **One flag or two?** Separate by default (`--storage-segment`, `--control-flow-segment`),
   because the fixed costs differ by roughly the program decode. Revisit after measuring the
   modes.

## Deferred

- **A payload-free transition guest.** After step 4, selection payloads are still decoded by the
  transition guest, because io (`verify_input_roots`) and recur (item width, source length) read
  them. If the storage guest committed, per binding, the small facts those checks need (the
  selected value's structural root, its `Ok`-inner root, a list's length and width), the
  transition guest could drop `storage_selection_witnesses`. That would take payload bytes, and
  the heap pressure behind the `List<MergeBucket>` failure, out of the sequential chain. It moves
  recur and io logic into the storage guest, so it should be argued on step 0's sizes.
- **Merging the control-flow and storage guests.** One guest per segment would pay the frame reads
  and one receipt once. The two share no checks and the control-flow guest's setup is the cost a
  merge would add to storage, so this is unlikely to pay. Listed because the host already plans
  both on the same window.

## Implementation record (2026-10-21)

Landed as designed, as one change rather than four merges, with the differences below.

**Layout.**
- `raster-core/src/segment_tree.rs`: `TreeDomain { leaf, node }` with `leaf_hash`, `root`, `path`
  and `root_from_path`, moved out of `control_flow.rs`. `control_flow::{entries_root, entry_path,
  root_from_path}` remain as wrappers over `CONTROL_FLOW_TREE`, whose domains are the ones it
  always used, so no control-flow root moved (`the_control_flow_tree_keeps_its_domains`).
- `raster-core/src/storage_check.rs`: `StorageState`, `StorageSegmentHeader`, `StorageStep`,
  `StorageStepEntry`, `StorageJournal`, `StorageEvidence`, `STORAGE_TREE`
  (`raster/storage/{entry,node}/v1`) and `selection_witnesses_commitment`
  (`raster/storage/selection-witnesses/v1`).
- `guest_common/src/storage.rs`: the check, moved from `transition/src/checks/store.rs`. Step 3's
  `verify_step(frontier, before, input) -> StorageState` became **`StorageCursor`** (`new`,
  `state`, `verify_step`): the cursor owns the frontier and index root, so `before` is always
  `cursor.state()` and can't disagree with the frontier it describes. The body of
  `verify_storage_transition` moved unchanged apart from its signature (it now returns only the
  index root after) and the anchor call in front of it.
- `guest_common/src/storage_fixtures.rs`, behind a `test-fixtures` feature: the store fixtures
  (`build_storage_context`, read and write witnesses, `tile_step_with_store_roots`), shared by
  both guests' tests instead of copied.
- `guests/storage`: `main.rs` reads the header, then one frame per step through
  `segment::verify_segment(header, frame::read)`, which takes a closure so tests can feed it a
  `Vec`.
- `transition/src/checks/store.rs` is now the consumer: `StorageFacts`, `verify_storage` and
  `assert_evidence_matches`, mirroring `checks::control_flow`.
- Host: `raster_prover::storage` (`storage_guest_image_id`, `plan_storage_segments`,
  `prove_storage_segment`, `ProvenStorage`), and `raster_prover::segments` with `SegmentLength`
  (`ControlFlowSegments`, renamed and moved) and `SegmentLengths { control_flow, storage }`.
  `step_transitions` and the CLI take one `SegmentLengths` instead of a growing list of lengths.
  CLI: `--storage-segment window|<n>`, next to `--control-flow-segment`. Chain evidence uses the
  default for both.

**Host walk.** The host appends `sha256(write.entry)` from each step's recorded write witness and
takes `index_root_after` from each record with roots. It checks nothing: the guest repeats every
step, and a wrong start is refused at the first step's roots.

**Tests.**
- Storage guest, 17: the 11 storage-only tests from the transition guest (4 witness-shape, 6
  `verify_storage_transition_*`, and `a_step_that_wrote_nothing_may_not_record_a_commitment`), each
  now carrying the input source witness its record commits to, because the check anchors it
  itself. New: the anchor, `a_segment_carries_the_store_from_step_to_step`,
  `one_segment_and_one_step_segments_commit_the_same_entries`, a start frontier with the right root
  at the wrong position (`position 1, [EMPTY_LEAF]` folds to the empty store's root; the first
  write then lands at log position 2 and the honest index witness is refused), a segment started
  from another store, and an empty segment.
- Transition guest, 151: `tests/store.rs` (18) mirrors `tests/control_flow.rs`, with one tamper
  test per entry field and three cases for the new equality: a committed entry with another
  selection commitment, an honest entry with one payload byte changed in the transition guest's
  map, and an extra unread witness in that map. Also another step's entry, a path from the
  control-flow tree, and a `before` differing only in log position. Two continuity tests for
  `storage_image_id`. The one test that drives storage next to recur (a site `Start` reading
  its source) keeps doing so through `StorageCursor`.
- `raster-core`: the tree tests moved to `segment_tree` (5), plus `storage_check` (4).
  `raster-prover` host: segment planning (3), `SegmentLength` (2) and the storage pin in
  `check_fraud_proof_journal`. The in-zkVM `transition_guest_*_authorization_*` tests now prove a
  real storage receipt as an assumption.
- All suites pass: the storage, transition and control-flow guests, `raster-core`, `raster-prover`
  and `raster-cli` (dev mode).

**What moved.** `TransitionInput` (`storage_witness` out, `storage` in), `Transition` (frontier
and root out, `StorageState` in), `TransitionJournal` (`storage_image_id`), and so the transition
guest's image id. `StepRecord`, the trace, `commit.bin` and `program_commitment` did not move.
`hello-tiles` rebuilt from this checkout writes a `Raster.lock` byte-identical to the committed
one, so no tile id moved either.

**End to end.** `hello-tiles`, `RISC0_DEV_MODE=1`, 32-step windows. `--fraud-step 27` (top level,
`[11]`) in four modes (`window`/`window`, `window`/`8`, `window`/`1`, `1`/`1` for control
flow/storage). `47` (a recur-sequence body, `[21, 2, 1]`) and `58` (nested, `[23, 5, 4, 1]`) in
the default mode. Each one commits, audits and generates a fraud proof, and
`examples/verify_fraud_proof` accepts the default-mode and `window`/`1` proofs with every inner
guest pinned.

## Measured after implementation (2026-10-21)

Dev mode, risc0 `SessionStats` per proof (temporary probe, not committed). `hello-tiles
--fraud-step 27`, the 32-step window (exec 16–47) control-flow §Segments measured. The baseline is
that section's `window` row at `0474a76`, the same window: the control-flow guest's 2.57M user
cycles match to the digit.

| mode (control flow / storage) | storage guest | transition guest | control-flow guest | all guests |
| --- | --- | --- | --- | --- |
| baseline, no storage guest | — | 45.96M / 63.17M / 69 | 2.57M / 3.41M / 4 | 48.53M / 66.58M / 73 |
| `window` / `window` (default) | 31.28M / 37.75M / 36 | 13.90M / 31.20M / 33 | 2.57M / 3.41M / 4 | 47.74M / 72.35M / 73 |
| `window` / `8` | 31.30M / 38.01M / 38 | 13.80M / 31.20M / 33 | same | 47.66M / 72.61M / 75 |
| `window` / `1` | 31.45M / 43.78M / 53 | 13.65M / 30.15M / 33 | same | 47.67M / 77.33M / 90 |
| `1` / `1` | 31.45M / 43.78M / 53 | 13.40M / 28.57M / 33 | 7.09M / 16.78M / 32 | 51.94M / 89.13M / 118 |

Each cell is user cycles / total cycles / risc0 segments, summed over the window.

`prompt-prepare --fraud-step 796`: the 16-step window ending at exec 1045 (`emit_merged_piece`,
`[5, 2, 4, 5]`), one full merge-loop iteration. Built against this checkout in a scratch copy of
the benchmark project (`prompt-prepare-benchmark` procedure). The baseline is control-flow
§Segments' `window` row at `0474a76`, 29.29M / 39.58M / 39. Its split by guest uses today's
control-flow guest (2.11M user, matching that section's 2.11M to the digit), so the baseline
transition row is derived, not measured on its own.

| mode (control flow / storage) | storage guest | transition guest | control-flow guest | all guests |
| --- | --- | --- | --- | --- |
| baseline, no storage guest | — | 27.18M / 36.96M / 36 | 2.11M / 2.62M / 3 | 29.29M / 39.58M / 39 |
| `window` / `window` (default) | 18.44M / 22.28M / 22 | 8.79M / 17.30M / 17 | 2.11M / 2.62M / 3 | 29.34M / 42.21M / 42 |
| `window` / `8` | 18.45M / 22.54M / 22 | 8.77M / 17.30M / 17 | same | 29.32M / 42.47M / 42 |
| `window` / `1` | 18.54M / 24.25M / 34 | 8.69M / 17.30M / 17 | same | 29.34M / 44.17M / 54 |
| `1` / `1` | 18.54M / 24.25M / 34 | 8.59M / 17.30M / 17 | 4.13M / 8.39M / 16 | 31.26M / 49.94M / 67 |

Every mode generated a fraud proof, and `verify_fraud_proof` accepted the default-mode one.

- **Step 0's answer: the storage check is most of a step.** 31.3M of the transition guest's 46.0M
  user cycles on `hello-tiles`, 18.4M of 27.2M on `prompt-prepare`: about 68% on both. The proposal expected "several times" control-flow's 1–2%, and the
  actual share is far larger. The two other windows agree: storage 30.6M and 30.4M against
  transition 15.2M and 14.9M (`--fraud-step 47` and `58`, default mode).
- **The sequential chain halves.** The transition guest, which proves one step per execution with
  each step depending on the last, goes from 46.0M to 13.9M user cycles (−70%), 63.2M to 31.2M
  total (−51%), and 69 to 33 segments (−52%) on `hello-tiles`. On `prompt-prepare`: 27.2M to 8.8M
  (−68%), 37.0M to 17.3M (−53%), 36 to 17 segments (−53%). It is now one segment per step plus
  one.
- **The window's total stays level.** `hello-tiles`: user cycles −1.6%, total cycles +8.7%,
  segments unchanged at 73. `prompt-prepare`: user +0.2%, total +6.6%, segments 39 → 42 (+7.7%). The total-cycle increase is the storage guest's paging: one execution touching a window's
  witnesses, 37.75M total for 31.28M user.
- **The storage guest's fixed work is negligible.** 32 executions cost 0.17M user cycles more
  than one (16 cost 0.10M more on `prompt-prepare`). Per-step mode still adds 17 and 12 segments
  (36 → 53, 22 → 34), since every execution is at least one segment, so `window` stays the
  default. `8` is within a segment or two of it.
- **What the halved chain buys depends on provers.** Storage receipts depend on no transition
  receipt. With two provers, the storage segment (36 and 22 segments) and the transition chain
  (33 and 17) could run side by side: roughly half the window's wall clock. On one GPU the segment
  count is the same or slightly higher (+0% and +7.7%), so expect about the same time. The host doesn't exploit this yet:
  `step_transitions` proves the storage segments, then the chain, in sequence.

**Not measured:** real GPU proving with recursion, where each transition step also resolves one
more assumption.

### Where the storage guest's cycles go (2026-10-21)

Temporary `env::cycle_count()` probes inside the storage guest, not committed. Default mode (one
storage segment per window), the same two windows. User cycles:

| | `hello-tiles`, 32 steps | `prompt-prepare`, 16 steps |
| --- | --- | --- |
| whole guest | 31.54M | 18.60M |
| **coordinate index, read membership** | 6.64M (23 proofs, 288k each) | 7.51M (26, 289k) |
| **coordinate index, write non-membership** | 9.64M (17, **567k** each) | 2.84M (5, **567k**) |
| **coordinate index, write membership** | 4.91M (17, 289k) | 1.44M (5, 289k) |
| *index proofs, subtotal* | *21.18M (67%)* | *11.79M (63%)* |
| frame read and decode, per step | 4.59M (144k / step) | 3.19M (200k / step) |
| append-log frontier roots (4 folds per step at ~27.5k) | 2.88M | 1.42M |
| append-log read paths | 0.61M | 0.69M |
| selection payload folds | 0.18M (23 folds, 1,343 bytes in all) | 0.36M (24, 2,135 bytes) |
| selection-map commitment, record hash, anchor, input-source commitment | 0.52M | 0.50M |
| journal root, commit, entries to stdout | 0.22M | 0.11M |

- **The 256-level coordinate index is two thirds of the guest.** Each proof folds 256 levels
  at about 1,130 cycles a level. A program holds hundreds to a few thousand objects, so about 10
  of those levels carry information. The frame decode is mostly the same proofs' siblings: 8 KB per
  proof, decoded byte by byte.
- **A non-membership proof costs twice a membership proof, for no reason.**
  `verify_coordinate_index_non_membership` (`raster-core/src/coordinate_index.rs:278`) calls
  `empty_hashes()`, which hashes the full 256-level empty-subtree table, to read one entry: the
  empty leaf, `sha256(EMPTY_LEAF_DOMAIN)`. That is 278k wasted cycles per write, 4.7M (15%) of
  `hello-tiles`' storage guest and 1.4M (8%) of `prompt-prepare`'s. Computing the leaf alone
  changes no commitment.
- **The append-log root is folded four times a step** (the state before, the check's own
  before-root, the root after the append, the state after) where one fold per write would do. A
  `StorageCursor` that caches its root saves about two thirds of 2.9M and 1.4M.
- **Selection payloads are negligible on these programs.** At most 178 bytes per fold. The
  "payload-free" item in §Deferred is not motivated by these windows.

What that leaves, by size and by cost:
1. **Free fixes, no commitment change:** compute only the empty leaf in non-membership, and cache
   the frontier root. Together about 22% of `hello-tiles`' storage guest and 14% of
   `prompt-prepare`'s.
2. **Cheaper levels, no commitment change:** `combine_node_hash` builds a `Vec` per level through
   `sha256(&[…])`. At about 1,130 cycles for two accelerated SHA blocks, most of a level is
   allocation and copying, not hashing. Fixed-size buffers would cut each level without touching
   any root.
3. **A shallower index, commitment change:** a sparse Merkle tree that stops at the depth where a
   key becomes unique (a compact sparse Merkle tree, `compact-coordinate-index`) needs about log₂(objects) levels per proof
   instead of 256. On these numbers that is most of the 21.2M and 11.8M, plus most of the frame
   decode. It changes the index root, so the recorder, `commit.bin` and the guest move together.
   It is also `storage-write-cost`'s Term 2 on the host side, where the same index is 93% of an
   append.

**Fix 1 landed (2026-10-21).** `verify_coordinate_index_non_membership` hashes only the empty leaf
(`empty_leaf_hash`, pinned to the bottom of `empty_hashes` by a test), and `StorageCursor` keeps
the append-log root, folding it once per write. No commitment moved, and both programs rebuild to
an unchanged lock. Both functions run only in the guests. Default mode, dev mode:

| | storage guest before | after | window, all guests, after |
| --- | --- | --- | --- |
| `hello-tiles`, 32 steps | 31.28M / 37.75M / 36 | **24.18M / 29.36M / 28** (−23% user) | 32.98M / 41.16M / 40 |
| `prompt-prepare`, 16 steps | 18.44M / 22.28M / 22 | **15.81M / 19.14M / 19** (−14% user) | 22.61M / 28.05M / 28 |

The predicted 22% and 14%. Against `0474a76`, before `storage-prover` and
`transition-segments`, a window is now 73 → 40 and 39 → 28 segments, with total cycles −38% and
−29%. Per-step storage (`--storage-segment 1`) on `hello-tiles`: 25.21M / 33.95M / 46. The index
proofs are still the largest share, so fix 3 (a shallower index) is where the rest is.
