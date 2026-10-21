# Proposal: `control-flow-prover` — prove one step's CFS conformance in its own guest

Status: **Implemented 2026-10-21.** Proposed 2026-10-19 at `c049b76`. Step 1 landed 2026-10-19,
steps 2–5 2026-10-20, step 6 2026-10-21 (§Implementation record, §Implementation record: step 6).
Step 0 and the cost of step 6 are measured on `hello-tiles` and `prompt-prepare`
(§Measured after implementation). All three open questions are settled. **Segments added
2026-10-21** (§Segments): one control-flow receipt covers a run of steps, the whole window by
default, with a Merkle root over per-step entries. That takes §Deferred's whole-window item. The
frame stack was profiled and not built, and GPU timing is still to run.

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

The continuity assertion is **required**. The pattern being copied lacked it until 2026-10-21:
nothing compared `input.authorization_image_id` with `prev_journal.authorization_image_id` on a
`Next` step, and no single-program verifier checked the final id.
[`authorization-image-id-unpinned`](../issues/authorization-image-id-unpinned.md) closed both
gaps, with `assert_authorization_continuity` in the guest and `verify_fraud_proof_receipt` on the
host. The new ID needs both: its own continuity assertion, and a pin in
`verify_fraud_proof_receipt`.

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

### Measured after implementation (2026-10-21)

Dev mode, user cycles from risc0's `SessionStats` per proof, plus `env::cycle_count()` inside the
step-5 guest around `verify_step`. Temporary probes, not committed. Two windows:

- `hello-tiles`, `--fraud-step 27`: 32 steps, exec 16–47, the window §Cost used.
- `prompt-prepare`, `--fraud-step 796`: 16 steps ending at exec 1045, `emit_merged_piece` at
  `[5, 2, 4, 5]`. That covers one full iteration of the merge loop, which is 85% of the trace's
  eligible steps. The benchmark's old `--fraud-step 1786` no longer exists: the trace now has
  1,621 eligible steps.

Each program was built against this checkout, so tile ids are its own. "Step 5" is HEAD
`6393fa8`, the step-5 guest plus `authorization-image-id-unpinned`.

**Step 0, completed.** The CFS check's share of a step, in the step-5 guest:

| per transition step | `hello-tiles` mean (max) | `prompt-prepare` mean (max) |
| --- | --- | --- |
| `verify_step` | 15,073 (50,206) | 28,764 (69,734) |
| `apply_verified_step` | 888,898 | 1,038,698 |
| before `apply_verified_step` | 484,641 | 606,869 |
| whole guest execution | 1,418,988 | 1,712,946 |
| CFS share of apply / of the step | 1.7% / 1.1% | 2.8% / 1.7% |

That is below the 39,030 cycles §Cost measured at `c049b76`. Steps 2 and 4 took chunking and the
witness-map scan out of the check, and step 3 put the anchor's hash in, but the probe doesn't
separate those effects. The decode cost of `input_sources_witnesses` can no longer be measured,
because step 4 removed the field.

**Step 6's cost.** The same windows, one guest per step (step 5) against two (step 6):

| window total | `hello-tiles` step 5 | step 6 | `prompt-prepare` step 5 | step 6 |
| --- | --- | --- | --- | --- |
| transition guest, user cycles | 45,768,923 | 45,668,260 | 27,596,540 | 27,040,992 |
| control-flow guest, user cycles | — | 6,397,786 (199,931 / step) | — | 3,779,902 (236,244 / step) |
| user cycles, both guests | 45.77M | 52.07M (+13.8%) | 27.60M | 30.82M (+11.7%) |
| total cycles, both guests | 63.70M | 77.33M (+21.4%) | 36.70M | 44.30M (+20.7%) |
| segments, both guests | 69 | 101 (+46%) | 38 | 52 (+37%) |

This confirms §Cost's prediction.
- **The control-flow guest is mostly fixed work.** It spends 200–236k user cycles per step to run
  a check that costs 15–29k. The rest is the program decode, `CfsCursor::new`, the input decode
  and the journal.
- **The transition guest barely shrinks** (−0.2% and −2.0%). Verifying the receipt and the
  equalities cost back most of what the check cost.
- **Segments grow by about one per step,** because every control-flow execution is at least one
  segment. On a GPU, prove time follows segments, so expect roughly +40% per window.

Step 6 is paid for by isolation, as §Cost said. The way to recover the cost is §Deferred:
one control-flow execution per window pays the fixed work once instead of 16–32 times.

## Migration

0. **Measure.** *Done: 2026-10-19 for hello-tiles (§Cost), 2026-10-21 for both programs
   (§Measured after implementation).* Wrap the five calls in
   `env::cycle_count()` on the `prompt-prepare` 16-step benchmark and the hello-tiles 32-step
   window. Record the CFS share of user cycles and the decode cost of `input_sources_witnesses`.
1. **Close the issue.** *Done 2026-10-19.* Change `source_witness_bytes` to commit the argument
   names. Add the §Reproduction tests from the issue as regression tests, written first and seen
   to fail before the fix.
2. **Move chunking** to the recur call site. *Done 2026-10-20.*
3. **Extract `verify_input_source_anchored`.** *Done 2026-10-20.*
4. **Narrow the scope-parent witness**, on both host and guest. *Done 2026-10-20.*
5. **Introduce `ControlFlowInput` and `verify_step`.** Split `cfs.rs` into `control_flow.rs` and
   `recur.rs`. Split the CFS tests in `tests.rs` out by the function under test. *Done
   2026-10-20.*
6. **Add the control-flow guest.** Move `frame.rs` and `merkle_tree.rs` into `guest_common`, and
   `control_flow.rs` and its tests into the new guest. Add `control_flow_image_id` to
   `PublicParams` and `TransitionJournal`, with the continuity assertion, and pin it in
   `verify_fraud_proof`. Add host proving. Add one tamper test per journal field: change it by one
   byte, and the transition guest must reject. For `input_source_commitment`, also test the
   case where the journal matches but the transition guest is given a different
   `input_source_witness`: one argument name changed, everything else the same. *Done 2026-10-21.*

Each of steps 1–5 leaves one guest and a passing suite. Stopping after step 5 is a valid end state.

## Implementation record (steps 2–5, 2026-10-20)

Landed as designed, with the differences below. Line numbers elsewhere in this document refer to
`c049b76` and are now stale: `cfs.rs` no longer exists.

- **Step 2.** The rule is `checks::recur::verify_recur_iteration_chunking(cfs, record, replay)`.
  It looks the site up itself and returns for any step that is not a recur iteration, so
  `apply_verified_step` calls it unconditionally, next to `advance_recur_progress`.
  `verify_step_record_inputs` keeps its early return for iterations and lost its `replay_journal`
  parameter. The six chunking tests now target the new function, and
  `verify_step_record_inputs_leaves_chunking_to_the_recur_call_site` pins the split: an iteration
  of a chunked site passes the binding check with no journal at all.
- **Step 3.** As written. `verify_step` calls the anchor first, and `io::verify_step_record` still
  calls it, so the hash runs twice per step until step 6.
- **Step 4.** Narrower than written, in two ways.
  - **Entry arguments lost their paths too.** The host built trace paths for every resolved
    source: `SequenceScope` parents, `PriorItemOutput` producers, and `ProgramStart` for every
    `EntryArgument`. The guest read only the first kind. `witness_record_inputs` and
    `resolve_inputs_sources` (with `flatten_binding`, `record_produces_item`,
    `program_start_index`, `frame_opening_index`) are replaced by `witness_sequence_scope_parents`.
    It mirrors the guest's guards, including `binds_sequence_scope`.
  - **The type changed.** `FraudEvidence.input_sources_witnesses` became
    `sequence_scope_parents: BTreeMap<u64, (StepRecord, StepRecordWitness)>`, keyed by the
    verifying step's `exec_index`. `TransitionInput.input_sources_witnesses` became
    `sequence_scope_parent: Option<(StepRecord, StepRecordWitness)>`.

  The guest's `find_map` filter became explicit assertions on the one parent it receives: kind
  `SequenceStart`, coordinates equal to the step's parent frame. The `exec_index` part of the old
  key is gone. A witness borrowed from another step is still refused, now by the fold against
  this step's root (`a_step_cannot_borrow_another_steps_witness_for_the_same_parent` was
  rewritten to show exactly that).

  One host behaviour changed: evidence for a window whose step reads a prior item with no producer
  in the trace used to panic in the host. It now builds, and the guest's storage check is what
  refuses the read (`a_window_over_an_unproduced_prior_item_builds_without_a_scope_parent`).
- **Step 5.** `ControlFlowInput` is **borrowed** (`ControlFlowInput<'a>`, all references) and lives
  in `checks/control_flow.rs`, not in `raster-core`. Inside the transition guest every field
  already exists in the step input or the live state, so an owned copy would clone the record and
  both `FnInput`s on every step. Step 6 needs the owned, serializable form in `raster-core` as
  sketched in §Step 6. `verify_step` now runs first in `apply_verified_step`, ahead of the other
  families, where `get_next_expected_coordinates` used to run last. Every check is an assertion
  and none of them mutates state another reads, so this changes only which assertion fires first
  on a step that fails several. Tests are split into `src/tests/mod.rs` (helpers, and tests that
  cross families), `tests/control_flow.rs` and `tests/recur.rs`. A test moved when every check
  function it reaches belongs to one family, so the site-`Start` tests that drive the control-flow,
  storage and recur checks together stay in `mod.rs`. Five `verify_step` tests were added.

**What moved.** `TransitionInput`'s serialized shape, and with it the transition guest's image
id. `StepRecord`, the trace, `commit.bin`, `program_commitment` and the tile image ids did not
move. The `raster-core` edits are a `TransitionInput` field and comments, and a `hello-tiles`
tile ELF built with and without them is byte-identical. The committed
`examples/hello-tiles/Raster.lock` fails the drift check in this checkout even with `raster-core`
at HEAD, so that drift predates this change. Tile ELFs embed absolute source paths, so a lock
generated from another checkout path does not match.

**Verified.** Transition guest: 173 tests (168 before, plus the five `verify_step` tests).
`raster-prover` host: `trace::` and `transition::tests::build_transition_input*`. End to end:
`examples/hello-tiles`, `RISC0_DEV_MODE=1`, 32-step windows: `--fraud-step 47` (a recur-sequence
body, `[21, 2, 1]`), `58` (nested sequences, `[23, 5, 4, 1]`) and `27` (top level, `[11]`) each
commit, audit and generate a fraud proof. Run against a lock rebuilt in this checkout, for the
reason above.

## Implementation record: step 6 (2026-10-21)

Landed as designed, with the differences below. *The same day, §Segments changed the journal (a Merkle root over one
entry per step), the guest's input (a segment), and the host (`ControlFlowWalk::input_for` and
`prove_control_flow_step` became `plan_control_flow_segments` and `ProvenControlFlow`, with every
segment proven before the chain). This record describes step 6 as it first landed.*

**Layout.**
- `raster-core/src/control_flow.rs` holds `ControlFlowInput` (owned, serializable) and
  `ControlFlowJournal` exactly as sketched in §Step 6, plus `expected_successors`. The module is
  `std`-gated like `transition`, so tile guests don't compile it.
- `guests/guest_common` holds `frame.rs`, `merkle_tree.rs` and also `input_source.rs`:
  `input_source_commitment` and `verify_input_source_anchored`, which both guests call. The
  transition guest imports `frame` and `merkle_tree` at its crate root, so its `crate::frame`
  paths are unchanged, and `checks::io` re-exports the two anchor functions.
- `guests/control_flow` is the new guest. `checks.rs` holds the conformance half moved from the
  transition guest, with `verify_step(cfs, &ControlFlowInput) -> Vec<CfsCoordinates>`.
  `tests.rs` holds the 50 control-flow tests and the fixtures they need (copied, not shared:
  the guests are separate crates). It adds
  `a_site_start_naming_its_producer_satisfies_its_bindings`, because the transition guest's
  `run_site_start` fixture no longer calls the binding check.

**Transition guest.**
- `checks/control_flow.rs` is now the consuming side. `verify_control_flow` does the unconditional
  `env::verify` and then `assert_journal_matches`, one equality per field the proposal lists.
- `PublicParams` reads `control_flow_image_id` as the frame after the transition image id.
  `TransitionJournal` commits it, and `assert_control_flow_continuity` holds it across `Next`
  steps, the rule `authorization-image-id-unpinned` added for the authorization id.
- `TransitionInput` lost `sequence_scope_witness` and `sequence_scope_parent` and gained
  `control_flow_journal`.

**Host.**
- `raster-prover/src/control_flow.rs` has `control_flow_guest_image_id`,
  `prove_control_flow_step`, and `ControlFlowWalk`, which produces each step's input in window
  order.
- One difference from §Step 6's host paragraph: the host does **not** run `verify_step`
  natively to get `expected_before`. The check is guest code with risc0 hashing and isn't linked
  into the host. The walk computes the successor set with `expected_successors`, which is
  `CfsCursor::try_get_next_coordinates` with `ProgramEnd` mapped to the empty set. It checks
  nothing: if the host's set disagrees with the guest's, the next step's `expected_before`
  equality refuses it.
- `step_transitions` proves each step's control-flow receipt just before the transition step and
  adds it as an assumption. They are proven one after another, not ahead of time or in parallel,
  though nothing prevents that.

**Pinning.** The relying party pins the id in `verify_fraud_proof_receipt` (added for
`authorization-image-id-unpinned`), not only in the example. `verify_fraud_proof.rs` goes through
that function.

**What moved.** The transition guest's image id, and `TransitionInput`'s and
`TransitionJournal`'s shapes. There is a new image id to pin: the control-flow guest's. The
trace, `commit.bin` and `program_commitment` did not move.

**Tests.**
- Control-flow guest: 51.
- Transition guest: 136. That is the non-control-flow tests, plus ten per-field tests on
  `assert_journal_matches` (one byte changed in each field, the `None`/`Some` halves, and the
  proposal's case of an honest journal with a transition witness whose argument is renamed), plus
  two continuity tests.
- Comparing test names against the step-5 suite: none dropped, 13 added.
- `raster-prover`: new `control_flow::tests` for the walk, `check_fraud_proof_journal` refuses an
  unknown control-flow guest, and the proving tests now prove a control-flow receipt too.

**End to end.** `examples/hello-tiles`, `RISC0_DEV_MODE=1`, 32-step windows: `--fraud-step 58`,
`47` and `27` each audit and generate a fraud proof, and `verify_fraud_proof` accepts the step-47
proof with all three inner guests pinned. `Raster.lock` passes the drift check unchanged, so the
`raster-core` additions moved no tile id.

**Cost.** Measured in cycles and segments on both programs. See §Measured after implementation.

## Segments (2026-10-21)

Step 6 proved one step per control-flow execution, and §Measured after implementation showed what
that costs: the guest's fixed work (200–236k cycles), and at least one segment, on every step. A
**segment** is a run of consecutive steps proven in one control-flow execution. The host picks
the length with `--control-flow-segment`: `window` (the default) for one receipt per window, or
`<n>` for one per `n` steps, where `1` is step 6's per-step mode. The guests, the journal, the
transition guest's check and the image ids are the same in every mode, so a relying party cannot
tell which produced a proof and doesn't need to.

**What the guest reads.** `ControlFlowSegment`: the trace frontier and the expected coordinates
before the segment's first step, then each step's record and witnesses (`ControlFlowStep`).
`checks::verify_segment` runs `verify_step` on each step in order. It derives every later step's
trace position and root by appending the records before it, the way the transition guest's
frontier grows, and its expected coordinates from the previous step's successors. Only the start
is host-supplied.

**What it commits.**
- **One `ControlFlowEntry` per step,** with exactly step 6's per-step fields: record hash,
  input-source commitment, trace position, trace root before, and expected coordinates before and
  after.
- **The journal commits only a Merkle root over them:** `ControlFlowJournal { program_commitment,
  first_position, entry_count, entries_root }`.
- **The tree.** Leaves are domain-separated hashes of each entry's postcard encoding
  (`ControlFlowEntry::leaf_hash`), and inner nodes have their own domain. An odd last node is
  carried up rather than duplicated, so `[a, b, c]` and `[a, b, c, c]` have different roots. The
  code is `entries_root`, `entry_path` and `root_from_path` in `raster-core::control_flow`, using
  `raster-core`'s hash, which is risc0's accelerated SHA inside a guest.
- **The entries go to the host on stdout,** not into the journal. They are untrusted there:
  `prove_control_flow_segment` refuses them unless they fold to the committed root, and every
  transition step checks its own again.
- **The journal is committed as raw postcard bytes** (`encode`, `env::commit_slice`), not with
  risc0's serde.

**What each transition step checks.** It gets `ControlFlowEvidence`: the journal, its own entry,
and that entry's path. It then:
1. verifies the receipt against the journal,
2. checks that its trace position falls in the segment (`index_of`),
3. checks that its entry folds along the path at that index to `entries_root`
   (`entry_is_committed_at`),
4. runs step 6's equalities on the entry.

Every step does all of this itself, even when several steps share a segment. Each transition step
is its own execution, handed its evidence by the host, so a check one step made on the segment
says nothing about another. A design that checked the segment's start once, at its first step,
would let a later step be given a different segment whose start is fabricated, and the
control-flow guest would then fold scope parents against a trace root of the prover's choosing.
Repeating the full equality per step is what makes the trace facts inside a segment safe to
derive rather than supply.

**Soundness.** The same as §Soundness, per entry. Each entry's position, root and expected set are
equated with the transition step's own state, so the derived values are checked, not trusted.
The path binds the entry to the receipt by collision resistance of the tree. The index is fixed
by the step's trace position, and the entry repeats that position, so an entry can't be
presented at another slot (`another_steps_entry_from_the_same_segment_is_refused`). The modes
agree by construction, and `one_segment_and_one_step_segments_commit_the_same_entries` pins it:
the window as one segment, and as one-step segments each started where the last ended, commit
identical entries.

**Host.** `plan_control_flow_segments` cuts the window and walks it, so each segment starts from
the frontier and successor set the previous one ended in. `ProvenControlFlow` proves every segment
before the transition chain starts, and `step_transitions` gives each step the covering receipt
and its evidence. Chain evidence (`chain fraud-prove`) uses the default.
`raster_prover::control_flow::ControlFlowSegments` is the setting, parsed from the flag.

**Cost.** Every segment pays the control-flow guest's fixed work and at least one segment of
proving. A transition step pays one entry and a path of log₂(n) hashes, whatever the segment's
length.

Measured 2026-10-21, dev mode, the windows of §Measured after implementation, totals over the
window for both guests. Step 5 is HEAD `6393fa8`, one guest per step.

| `hello-tiles`, 32 steps | user cycles | total cycles | segments |
| --- | --- | --- | --- |
| step 5 | 45.77M | 63.70M | 69 |
| `1` (per step) | 52.80M (+15.4%) | 79.95M (+25.5%) | 101 (+46%) |
| `8` | 48.87M (+6.8%) | 67.37M (+5.8%) | 73 (+5.8%) |
| `window` (default) | 48.53M (+6.0%) | 66.58M (+4.5%) | 73 (+5.8%) |

| `prompt-prepare`, 16 steps | user cycles | total cycles | segments |
| --- | --- | --- | --- |
| step 5 | 27.60M | 36.70M | 38 |
| `1` (per step) | 31.21M (+13.1%) | 44.83M (+22.1%) | 52 (+37%) |
| `8` | 29.40M (+6.5%) | 39.58M (+7.8%) | 40 (+5.3%) |
| `window` (default) | 29.29M (+6.1%) | 39.58M (+7.8%) | 39 (+2.6%) |

- **The whole window is the cheapest or tied** on both programs, which is why it is the default.
- **Finding an entry costs a transition step about 7k cycles.** That's the difference between
  window-mode and per-step transition totals on `hello-tiles`.
- **Two earlier designs were worse.** Before the Merkle root, a step decoded, re-encoded and
  hashed the whole journal: about 78k cycles per step at 32 entries, which made eight-step
  segments the better default. Before the postcard journal it was about 141k, because risc0's
  serde writes every byte as a word.
- **Per step is the most expensive mode** and is kept for comparison and debugging.

**Where the control-flow guest's cycles go** (window mode, temporary `env::cycle_count()`
probe):

| | `hello-tiles` (32 steps) | `prompt-prepare` (16 steps) |
| --- | --- | --- |
| whole guest | 2.57M | 2.11M |
| setup: program and segment decode, `CfsCursor::new` | 691k | 859k |
| `verify_step`, all steps | 488k | 465k |
| of which scope-parent folding (5 steps each) | 193k | 231k |
| rest of the step loop: record and leaf hashing, trace roots | 1.23M | 0.69M |
| root, commit, entries to stdout | 157k | 97k |

So **the frame stack is not worth building** at these sizes. It removes only the scope-parent
share, 7–11% of the control-flow guest and under 1% of a window's total cycles. The larger share
of the step loop is recomputing the trace root before each step (a 32-level fold). That could be
made incremental if the control-flow guest ever matters more than it does now.

**Not measured: real proving.** These are dev-mode user cycles and segment counts. GPU proving
adds the recursion work they don't show: lifting each control-flow receipt, and resolving the
control-flow assumption once per transition step in every mode. A real run per mode on
`prompt-prepare` would confirm the ranking. It wasn't run on 2026-10-21 because the GPU was busy
with other work.

## Deferred

- **Whole-window control flow.** *Done as §Segments. The frame stack was profiled and not
  built.* One control-flow execution per window instead of per step. The scope-parent fold could
  then become a frame stack: the window opens with the `SequenceStart` records of the frames
  already open, each proven once against the starting root, and they are pushed and popped as
  the window runs. That would remove per-step Merkle paths. §Segments measured the folding at
  7–11% of the control-flow guest and under 1% of a window, so it stays a per-step fold.
- **Other families.** Whether `store`, `io`, `drafts`, recur and `entrypoint` follow is the
  question [`transition-split`](./transition-split.md) asks. This proposal does not answer it.

## Open questions

1. **Pinning.** Pass the ID in as input and commit it to the journal (recommended above), or
   compile it in. Compiling it in depends on whether guest image IDs are reproducible across
   machines in this build setup. *Resolved 2026-10-21: passed in and
   committed, held continuous, and pinned in `verify_fraud_proof_receipt`.*
2. **Should `exec_index` move?** It is a function of trace position, not of the CFS. It is here
   because it is in `cfs.rs` today and costs one comparison. If the trace checks are ever
   extracted, it goes with them. *Settled 2026-10-21: it stays.* The control-flow journal commits
   `trace_position`, and the transition guest equates it, so the check reads no input it
   wouldn't have anyway. Moving it would add nothing. Revisit only if the trace checks become
   their own part ([`transition-split`](./transition-split.md)'s `trace`).
3. **`ty` in the commitment.** Step 1 leaves it out (§Step 1). If a future check reads `ty`, it
   has to be committed then, or the same gap reopens for that field. *Settled 2026-10-21: no
   action.* Nothing reads `ty`. The rule stands: a change that makes a check read `ty` must add it
   to `FnInput::source_witness_bytes` in the same change.
