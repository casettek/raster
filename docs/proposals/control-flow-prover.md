# Proposal: `control-flow-prover` — prove one step's CFS conformance in its own guest

Status: **Proposed 2026-10-19** at `c049b76`. **Step 1 implemented 2026-10-19.** Steps 2–6 are
open. Steps 1–5 of §Migration change only today's transition guest, and each one can land on its
own. Step 6 adds the new guest.

Scope: **one transition step.** The control-flow guest proves the CFS checks for exactly the step
that one transition-guest execution proves, and the transition guest consumes that receipt.
Proving a whole window in one control-flow execution is deferred (§Deferred).

Related:
- [`fn-input-arg-names-unbound`](../issues/fn-input-arg-names-unbound.md): a soundness gap in
  the checks this proposal moves. Step 1 closes it before anything moves.
- [`transition-split`](./transition-split.md): splits the transition guest into one part per check
  family, each over a whole window. This proposal is narrower. It extracts one check family at the transition
  guest's current granularity, and it neither depends on nor commits to that design.
- [`sequence-scope-forbids-narrowing`](../issues/sequence-scope-forbids-narrowing.md) (resolved),
  [`dynamic-index-selection`](./dynamic-index-selection.md): the binding rules being moved,
  unchanged.
- [`program-identity`](./program-identity.md): `program_commitment`. Both guests derive it from
  the same frame bytes, and the transition guest checks that they agree.

## Problem

`LiveTransition::apply_verified_step` (`guests/transition/src/fraud_proof.rs:637`) runs every
check family on one step, in one function, over one shared state. Five of its calls make up the
CFS check:

```
fraud_proof.rs:647  verify_exec_index(frontier.position(), record)
fraud_proof.rs:648  verify_sequence_id(cfs, record)
fraud_proof.rs:651  verify_sequence_scope_parent(cfs, record, scope_witness, input_sources_witnesses, frontier_root)
fraud_proof.rs:658  verify_step_record_inputs(cfs, record, input_source_witness, scope_witness, replay_journal)
fraud_proof.rs:731  get_next_expected_coordinates(cfs, record, next_expected_coordinates)
```

The CFS check could be a self-contained question: does this record sit where the program says
it can, and do its arguments come from where the program says they do? It is not
self-contained today, for four reasons. All four are hidden by sharing one function:

1. **It reads the replay journal.** `verify_step_record_inputs` calls
   `verify_recur_iteration_chunking` (`cfs.rs:339`), which reads `replay.recur.position`, an
   output of `env::verify` in `checks::io`. Chunking is a recur rule. It sits in the CFS check
   only because the recur-iteration early return is there.
2. **It relies on an anchor checked by another family.** Every binding check reads the step's
   `input_source_witness`. The only check that this witness is the one the record commits to is
   in `io::verify_step_record` (`io.rs:72-89`). The CFS check is sound only because `io` runs in
   the same execution.
3. **Its input carries witnesses it never reads.** `input_sources_witnesses` is a
   `HashMap<(u64, StepRecord), StepRecordWitness>`. The host builds trace-inclusion paths for
   `SequenceScope` parents **and** `PriorItemOutput` producers (`raster-prover/src/trace.rs:777`,
   `:810`). The guest's only reader keeps a `SequenceStart` at the parent coordinates
   (`cfs.rs:694`), and never reads the `PriorItemOutput` paths. `PriorItemOutput` is checked
   statically against coordinates (`cfs.rs:892-978`), which is correct, since the storage check
   proves the object at those coordinates. The paths are dead weight in every step's input.
4. **A soundness gap**, [`fn-input-arg-names-unbound`](../issues/fn-input-arg-names-unbound.md):
   the position-to-binding map (`args[i].name`) is outside `input_source_commitment`, and the
   caller's map in the scope witness is pinned by nothing.

Because of 1 and 2, the CFS checks cannot be tested, reviewed, or proven apart from the rest of the
guest. Because of 3, every step pays to decode data nobody uses. Item 4 has to be fixed regardless.

## Goal

1. Make the CFS check a **pure function of the program, one record, and witnesses anchored to
   that record or to the trace root**. Do this inside today's guest first.
2. Then move that function into a **control-flow guest** that proves one step. The transition
   guest verifies the receipt and checks that the journal agrees with the transition guest's own
   state.

Non-goals:
- No whole-window control-flow proving, frame stack, or split of the other check families (§Deferred).
- No change to what any CFS rule accepts, beyond step 1's fix and the narrower scope-parent input
  in step 4.
- No change to trace shape, `StepRecord`, the fingerprint, or the `Finished` journal's meaning.
  Step 1 changes the bytes of `input_source_commitment`, so trace leaves and `commit.bin`
  fixtures change once.

## What the CFS check reads

After steps 1–5, this is the complete input:

| input | source | how it is trusted |
| --- | --- | --- |
| CFS (`CfsCursor`) | `program.bin` frame | `program_commitment`, which the transition guest equates |
| `step_record` | host | `hash_trace_item(record)`, which the transition guest equates. It also goes into the trace leaf |
| `trace_position`, `trace_root_before` | the transition guest's frontier before the append | transition guest equates both |
| `expected_before: Option<Vec<CfsCoordinates>>` | the transition guest's `next_expected_coordinates` | transition guest equates it |
| `input_source_witness: Option<FnInput>` | host | **anchored**: its commitment equals `record.input_source_commitment` (checked in the control-flow function) |
| `sequence_scope_witness: Option<FnInput>` | host | **anchored**: its commitment equals the scope parent's `input_source_commitment` |
| `scope_parent: Option<(StepRecord, StepRecordWitness)>` | host | **anchored**: folds to `trace_root_before` |

And the complete output: `expected_after: Vec<CfsCoordinates>`, which is empty after `ProgramEnd`.

It does not read: input/output bytes, storage roots, storage or selection witnesses, the replay
journal, draft state, or recur progress.

The rules themselves (successor, kind matches item, sequence ID, exec index, the five binding
forms, scope parent) are unchanged. They are described at the call sites listed in §Problem.

## Design

### Steps 1–5: inside today's transition guest

Each step keeps a single guest and can be merged on its own.

**Step 1: close `fn-input-arg-names-unbound`.** *Implemented 2026-10-19.* Use direction 1 from
the issue: commit the names.

```rust
// raster-core/src/trace.rs
impl FnInput {
    pub fn source_witness_bytes(&self) -> Vec<u8> {
        let names: Vec<&str> = self.args.iter().map(|arg| arg.name.as_str()).collect();
        postcard::to_allocvec(&(&self.values, &names, &self.storage)).unwrap_or_default()
    }
}
```

The hash stays where it is on each side: the recorder hashes these bytes with `sha2`
(`recorder.rs:267`), and the guest with risc0's accelerated SHA (`checks::io::input_source_commitment`,
through `merkle_tree::sha256_bytes`). Moving the hash into `raster-core` would put software SHA
into the guest. The preimage is what both sides must agree on, and it lives in one function.

Direction 1 is chosen because it is one shared function. Recorder and guest cannot disagree, and
it needs no type or CFS change. The cost is one change to every `input_source_commitment` and
the fixtures that depend on them. Direction 2 (`StorageBinding(name)`) is equally sound and
touches 14 match sites. Direction 3 does not cover recur-sequence iteration frames.

Only `name` is committed, not `ty`. Nothing in the guest reads `ty`. It is produced by
`stringify!` over the macro's tokens (`raster-macros/src/recur.rs:713`), and Rust does not
guarantee that output stays the same across compiler versions. Committing it would make
`commit.bin` depend on the compiler for a field that nothing checks.

**Step 2: move chunking to the recur call site.** Remove `verify_recur_iteration_chunking` from
`verify_step_record_inputs` (`cfs.rs:339`), keep the early `return`, and drop the
`replay_journal` parameter. Call it from `apply_verified_step` right before
`advance_recur_progress`, which already takes the replay journal. The rule and the step that
fails it stay the same.

**Step 3: make the input-source anchor its own call.** Extract `io.rs:72-89` into
`checks::io::verify_input_source_anchored(record, witness)`. `io::verify_step_record` keeps
calling it, and the CFS group calls it as well. The hash runs twice in one execution while both
calls live there. After step 6 each guest runs it once.

**Step 4: narrow the scope-parent witness.** Replace `input_sources_witnesses` in
`TransitionInput` with `sequence_scope_parent: Option<(StepRecord, StepRecordWitness)>`. The host
picks the one entry the guest's `find_map` (`cfs.rs:694-711`) would have picked, and
`witness_record_inputs` stops building `PriorItemOutput` paths. `verify_sequence_scope_parent`
keeps every assertion: the parent is a `SequenceStart`, it sits at the step's parent coordinates,
it folds to the root, and its commitment equals the scope witness's.

**Step 5: group the calls behind one function with an explicit output.**

```rust
// guests/transition/src/checks/control_flow.rs (the conformance half of cfs.rs, lines 1-1031)
pub fn verify_step(cfs: &CfsCursor, input: &ControlFlowInput) -> ControlFlowOutcome {
    let record = &input.step_record;
    io::verify_input_source_anchored(record, input.input_source_witness.as_ref());
    verify_exec_index(input.trace_position, record);
    verify_sequence_id(cfs, record);
    verify_sequence_scope_parent(
        cfs, record,
        input.sequence_scope_witness.as_ref(),
        input.sequence_scope_parent.as_ref(),
        &input.trace_root_before,
    );
    verify_step_record_inputs(
        cfs, record,
        input.input_source_witness.as_ref(),
        input.sequence_scope_witness.as_ref(),
    );
    let next = get_next_expected_coordinates(cfs, record, input.expected_before.as_ref());
    ControlFlowOutcome {
        expected_after: if matches!(record.kind, StepKind::ProgramEnd(_)) { Vec::new() } else { next },
    }
}
```

`apply_verified_step` builds a `ControlFlowInput` from its own state and the step's witnesses,
calls `verify_step`, and stores `expected_after`. Everything the function reads is in its
argument, so no other shared state can leak in. The recur half of `cfs.rs` (lines 1032–1662,
`authenticated_source_len` onward) becomes `checks/recur.rs`. The two halves share no helpers.

After step 5 the guest's behaviour is the same as today's, except for step 1's fix. The CFS check
is now a function that could run anywhere.

### Step 6: the control-flow guest

```rust
// raster-core/src/control_flow.rs (shared by host and both guests)
pub struct ControlFlowInput {
    pub step_record: StepRecord,
    pub trace_position: u64,
    pub trace_root_before: Vec<u8>,
    pub expected_before: Option<Vec<CfsCoordinates>>,
    pub input_source_witness: Option<FnInput>,
    pub sequence_scope_witness: Option<FnInput>,
    pub sequence_scope_parent: Option<(StepRecord, StepRecordWitness)>,
}

/// Everything in `ControlFlowInput` the transition guest also holds, and the outcome.
/// Data both guests use is committed here, directly or as a hash. Data only this
/// guest uses (the scope witness and its parent) is absent: it was checked
/// against `trace_root_before`, which is here.
pub struct ControlFlowJournal {
    pub program_commitment: Vec<u8>,
    pub step_record_hash: Vec<u8>,     // hash_trace_item(step_record)
    /// `input_source_commitment(w)` of the `input_source_witness` the checks ran on,
    /// computed in this guest. `None` exactly when the step carries no witness.
    pub input_source_commitment: Option<Vec<u8>>,
    pub trace_position: u64,
    pub trace_root_before: Vec<u8>,
    pub expected_before: Option<Vec<CfsCoordinates>>,
    pub expected_after: Vec<CfsCoordinates>,
}
```

`input_source_commitment` is the one witness both guests read. `io`, `store` and recur use the
same `FnInput` that the binding checks used. It is also implied by the record:
`verify_input_source_anchored` ties the witness to `record.input_source_commitment` in both
guests. So this field adds no new guarantee. It is there so that "both guests used the same
arguments" is one equality the reader can see in the transition guest, instead of a conclusion
drawn from two separate checks against the same record field. It costs 32 bytes per journal.

```rust
// guests/control_flow/src/main.rs
fn main() {
    let program_bytes = frame::read_bytes();
    let program_commitment = commitment_of_bytes(&program_bytes).to_vec();
    let program = ProgramDefinition::decode(&program_bytes)
        .expect("host must supply a valid ProgramDefinition frame");
    let cfs = CfsCursor::new(program.cfs);
    let input: ControlFlowInput = frame::read();

    let outcome = checks::verify_step(&cfs, &input);   // the step-5 function, moved

    env::commit(&ControlFlowJournal {
        program_commitment,
        step_record_hash: hash_trace_item(&input.step_record).to_vec(),
        input_source_commitment: input.input_source_witness.as_ref().map(input_source_commitment),
        trace_position: input.trace_position,
        trace_root_before: input.trace_root_before,
        expected_before: input.expected_before,
        expected_after: outcome.expected_after,
    });
}
```

In the transition guest, the `verify_step` call from step 5 becomes a receipt check plus
equalities. Each equality covers one input that both guests hold. On one side is the value the
receipt says the control-flow guest used. On the other is the value the transition guest uses:

```rust
// apply_verified_step
let cf = &input.control_flow_journal;
env::verify(params.control_flow_image_id, &risc0_zkvm::serde::to_vec(cf).unwrap())
    .expect("Failed to verify control-flow journal");
assert_eq!(cf.program_commitment, params.program_commitment);
assert_eq!(cf.step_record_hash, hash_trace_item(&input.step_record));
assert_eq!(
    cf.input_source_commitment,
    input.input_source_witness.as_ref().map(input_source_commitment),
    "control-flow and transition checks ran on different step arguments",
);
assert_eq!(cf.trace_position, u64::from(self.frontier.position()));
assert_eq!(cf.trace_root_before, frontier_root(&self.frontier));
assert_eq!(cf.expected_before, self.next_expected_coordinates);
// … other families unchanged …
self.next_expected_coordinates = Some(cf.expected_after.clone());
```

`TransitionInput` loses `sequence_scope_witness` and `sequence_scope_parent`, which only the CFS
check read, and gains `control_flow_journal`. `input_source_witness` stays, because `io`, `store`
and recur read it too.

**Host.** Every `ControlFlowInput` in a window can be computed before any proving starts: the
trace frontier advances by `hash_trace_item` over the window's records, and `expected_before`
comes from the previous step's outcome, computed natively with the same `verify_step`. So the
`N` control-flow receipts do not depend on any transition receipt. They can be proven up front or
alongside the transition chain, and each one is added as an assumption to its transition step
next to the authorization and replay receipts (`raster-prover/src/transition.rs:292-301`).

```
crates/raster-core/src/control_flow.rs          # ControlFlowInput, ControlFlowJournal
crates/raster-prover/guests/guest_common/       # new lib crate, no main: shared by both guests
└── src/
    ├── lib.rs
    ├── frame.rs                                # moved from transition: framed postcard reader
    └── merkle_tree.rs                          # moved from transition: hash_trace_item, frontier_root, …
crates/raster-prover/guests/control_flow/       # new guest
├── Cargo.toml
└── src/
    ├── main.rs
    ├── checks.rs                               # the conformance half of cfs.rs, moved from transition
    └── tests.rs                                # the CFS tests from transition's tests.rs, moved
crates/raster-prover/guests/transition/src/checks/
├── recur.rs                                    # the recur half of cfs.rs
└── (cfs.rs removed)
crates/raster-prover/src/control_flow.rs        # host: build inputs, prove one step
```

`frame.rs` and `merkle_tree.rs` (`hash_trace_item`, `combine_merkle_level`, `frontier_root`,
`sha256_bytes`) move into a small guest-side library crate that both guests depend on by path.
They can't go into `raster_core::merkle`: the guest's hashing uses risc0's accelerated SHA
(`Risc0Sha256`), and `raster-core` uses software `sha2`. A guest-side crate also leaves
`raster-core` untouched, so tile image IDs don't move.

### Image-id pinning

The transition guest has to accept only the real control-flow guest. **Recommended for this
step: the pattern `authorization_image_id` already uses.** `PublicParams::read` reads
`control_flow_image_id` as one more frame. `TransitionJournal` commits it.
`verify_previous_journal` asserts it is unchanged across the window. The relying party pins it
wherever it pins `TRANSITION_GUEST_ID`: today that is only
`raster-prover/examples/verify_fraud_proof.rs`, so step 6 adds a library function
(`control_flow_guest_image_id()`, next to `authorization_guest_image_id()` in
`raster-prover/src/authorization.rs:28`) and the example checks the journal against it.
`chain_fraud` is not involved: it never verifies a transition receipt (its evidence is `Link`,
which checks authorization only, or `Shape`), so its journal doesn't carry the ID.

The continuity assertion is **required**, and the pattern being copied lacks it: nothing compares
`input.authorization_image_id` with `prev_journal.authorization_image_id` on a `Next` step
(`fraud_proof.rs:245-250`). Only the last step's ID reaches the relying party. That is outside
this proposal's scope and should get its own issue. The new ID must not repeat it.

The alternative is to compile the ID into the transition guest. That keeps `TransitionJournal`'s
shape, but it needs a two-pass `risc0_build`, or a checked-in constant pinned by a test. A
checked-in constant is fragile unless guest builds are reproducible across machines (§Open
questions).

## Soundness

**Claim:** after step 6, the transition guest accepts a step exactly when the step-5 guest would
have accepted it.

In step 5, `verify_step` runs on a `ControlFlowInput` whose fields come from two places:
- **The guest's own state**: `program_commitment`, the record, frontier position and root,
  `next_expected_coordinates`. In step 6, each of these is a journal field, and the transition
  guest asserts it equals its own value. The record is compared by `hash_trace_item`, the same
  hash the trace leaf uses, so collision resistance gives the same record.
- **Host witnesses**: `input_source_witness`, `sequence_scope_witness`, `sequence_scope_parent`.
  Inside `verify_step` each of these is checked against a field in the first group: the record's
  commitment, the parent's commitment, and the trace root. So a witness the control-flow guest
  accepted under the equated fields is one the step-5 guest would have accepted.

`input_source_witness` is the only witness the transition guest also reads (`io`, `store`,
recur). Both guests hash it, and `input_source_commitment` compares the two hashes directly.
Collision resistance then makes the two copies the same `FnInput`, and after step 1 that includes
`args`. The record anchor gives the same conclusion on its own, so the equality is redundant by
design. If either anchor is ever weakened, this one still holds.

The output, `expected_after`, is committed by the receipt and stored as-is. Steps 2–4 remove
every other route by which the CFS check read shared state, so the journal is the CFS check's
entire interface. The hazard to watch for is a future edit that reads something new inside
`verify_step` without adding it to `ControlFlowInput`. The function signature makes that a
compile error: `verify_step` only sees its argument.

A missing control-flow receipt is not possible: the `env::verify` call is unconditional.

## Cost

**Measured 2026-10-19 (step 0)**, dev mode, `examples/hello-tiles`, a 32-step window
(`--fraud-step 27`), with step 1 applied. `env::cycle_count()` was wrapped around the five CFS
calls in `apply_verified_step` (the probe was not committed). User cycles per transition step:

| | mean | max | window total |
| --- | --- | --- | --- |
| the five CFS calls | 39,030 | 74,232 | 1,248,981 |
| `apply_verified_step` (all check families) | 884,779 | 1,745,666 | 28,312,938 |
| whole guest execution | 1,411,160 | 2,526,267 | 45,157,134 |
| before `apply_verified_step` (program decode, input decode, auth and previous-journal verify) | 495,241 | | |

The CFS check is **4.4%** of `apply_verified_step` and **2.8%** of a step. The fixed work before
any check runs is about **12×** the CFS check. Step 6 adds a second guest per step that pays its
own share of that fixed work (program decode, `CfsCursor::new`, input decode) in exchange for
removing about 39k cycles. On this program, step 6 costs more cycles than it removes. It can only
be justified by isolation, not by speed. The whole-window design in §Deferred pays the fixed
work once per window instead of once per step, which changes this ratio.

The decode cost of `input_sources_witnesses` could not be separated from the rest of the input
decode with this probe.

| per step | removed from the transition guest | added |
| --- | --- | --- |
| CFS lookups, binding checks, scope-parent fold | yes | moved to the control-flow guest |
| decoding `sequence_scope_witness` and `sequence_scope_parent` | yes | moved |
| decoding `PriorItemOutput` paths (step 4) | yes, everywhere | — |
| `program.bin` decode and `CfsCursor::new` | — | once more, in the control-flow guest |
| `env::verify` of the control-flow journal | — | one more assumption to resolve in the transition receipt |
| receipts | — | one control-flow receipt per step |

On one GPU this costs more in total than today. The added fixed overhead per step (a second
program decode, guest start-up, lift and resolve) is likely larger than the CFS cycles removed.
That is why steps 1–5 are worth doing on their own. Step 6 is justified by isolation (separate
tests, separate review, a guest that is about one question), by proving control-flow receipts
ahead of or alongside the transition chain, and as the base for §Deferred. It is not justified
by speed.

The first table above is step 0's measurement. `prompt-prepare` on the GPU has not been measured.

## Migration

0. **Measure.** *Done 2026-10-19 for hello-tiles in dev mode (§Cost).* Wrap the five calls in
   `env::cycle_count()` on the `prompt-prepare` 16-step benchmark and the hello-tiles 32-step
   window. Record the CFS share of user cycles and the decode cost of `input_sources_witnesses`.
1. **Close the issue.** *Done 2026-10-19.* Change `source_witness_bytes` to commit the argument
   names. Add the §Reproduction tests from the issue as regression tests, written first and seen
   to fail before the fix.
2. **Move chunking** to the recur call site.
3. **Extract `verify_input_source_anchored`.**
4. **Narrow the scope-parent witness**, on both host and guest.
5. **Introduce `ControlFlowInput` and `verify_step`.** Split `cfs.rs` into `control_flow.rs` and
   `recur.rs`. Split the CFS tests in `tests.rs` out by the function under test.
6. **Add the control-flow guest.** Move `frame.rs` and `merkle_tree.rs` into `guest_common`, and
   `control_flow.rs` and its tests into the new guest. Add `control_flow_image_id` to
   `PublicParams` and `TransitionJournal`, with the continuity assertion, and pin it in
   `verify_fraud_proof`. Add host proving. Add one tamper test per journal field: change it by one
   byte, and the transition guest must reject. For `input_source_commitment`, also test the
   case where the journal matches but the transition guest is given a different
   `input_source_witness`: one argument name changed, everything else the same.

Each of steps 1–5 leaves one guest and a passing suite. Stopping after step 5 is a valid end state.

## Deferred

- **Whole-window control flow.** One control-flow execution per window instead of per step. The
  scope-parent fold can then become a frame stack: the window opens with the `SequenceStart`
  records of the frames already open, each proven once against the starting root, and they are
  pushed and popped as the window runs. That removes per-step Merkle paths. It needs a journal
  that commits to the window's records rather than one record, so it changes the interface this
  proposal defines. Worth a proposal of its own once step 6's numbers exist.
- **Other families.** Whether `store`, `io`, `drafts`, recur and `entrypoint` follow is the
  question [`transition-split`](./transition-split.md) asks. This proposal does not answer it.

## Open questions

1. **Pinning.** Pass the ID in as input and commit it to the journal (recommended above), or
   compile it in. Compiling it in depends on whether guest image IDs are reproducible across
   machines in this build setup.
2. **Should `exec_index` move?** It is a function of trace position, not of the CFS. It is here
   because it is in `cfs.rs` today and costs one comparison. If the trace checks are ever
   extracted, it goes with them.
3. **`ty` in the commitment.** Step 1 leaves it out (§Step 1). If a future check reads `ty`, it
   has to be committed then, or the same gap reopens for that field.
