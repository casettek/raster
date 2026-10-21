# Proposal: `storage-prover` — prove the storage transition of a segment of steps in its own guest

Status: **Proposed 2026-10-21** at `0474a76`. Not measured. Step 0 (§Migration) measures the
storage check's share of a step and has to come first.

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

**Not measured.** What changes per step:

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
