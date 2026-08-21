# Proposal: Merge `SequenceStart(main)` + `Entrypoint` into a single `ProgramStart` step

Status: proposed (2026-07-16)

## Problem

Today a program's trace opens with **two** steps expressing one concept — "the user
program starts with its authorized external data":

1. `StepKind::SequenceStart` for `main` at coordinates `[]` — a **vacuous** step:
   - its input commitments are empty (the `#[sequence]` macro clears `main`'s signature
     in `crates/raster-macros/src/lib.rs`, because `main` has no caller),
   - the transition guest **skips** input verification for empty coordinates
     (`guests/transition/src/checks/cfs.rs`, which carries a standing TODO about exactly
     this "external input" case),
   - it carries no storage roots and is verified against nothing.
2. `StepKind::Entrypoint` at coordinates `[0]` — the step that actually loads the
   manifest-authorized entry arguments into storage and is checked against the
   authorization journal (`checks/entrypoint.rs`).

Because a fraud-proof window can legally open **between** these two steps, the guest
needs `EntrypointAuthorization::Pending` plus the `assert_discharged()` deadline at
`Finished` (`fraud_proof.rs`) — i.e., there exists a valid chain state whose storage has
**no tie to the public manifest yet**. The CFS also carries a synthetic
`SequenceChildItem::Entrypoint` item prepended at index 0 of `main`, forcing the
offset-by-1 hack in `FlowResolver::resolve_with_entry_arguments` and special cases
throughout `CfsCursor` (`"Entrypoint coordinates cannot have nested child coordinates"`,
`"__entrypoint"` naming) and the guest's `record_matches_item`.

## Goal

One first event that starts the user program with all external source data preloaded
into storage in an authorized way, so storage authorization is established at trace
index 0 and preserved invariantly through the whole execution — and a cleaner,
easier-to-reason-about model of how a raster program starts.

Design decisions taken:

- **Full merge at the root coordinate `[]`** (not the conservative variant that keeps
  the CFS item at `[0]` and only drops `main`'s `SequenceStart`).
- **`ProgramStart` is always emitted**, even when `main` declares no entry arguments,
  so every trace has one uniform first step and one uniform genesis rule.

## Design

### 1. Trace model (`crates/raster-core/src/trace.rs`)

Replace both `StepKind::Entrypoint(EntrypointStep)` and the `main` use of
`StepKind::SequenceStart` with one new kind, recorded at coordinates `[]` as the trace's
first step (`exec_index` 1):

```rust
StepKind::ProgramStart(ProgramStartStep)

pub struct ProgramStartStep {
    /// Declared entry-argument names, in CFS declaration order. Empty when
    /// main declares none.
    pub entry_arguments: Vec<String>,
    /// Struct-of-commitments root over the authorized per-argument
    /// commitments — the commitment of the single combined entry object
    /// written at coordinates `[]`. Empty when there are no arguments
    /// (no write happens).
    pub output_commitment: Vec<u8>,
    /// Genesis roots -> roots containing the entry object (or unchanged
    /// when there are no arguments).
    pub storage: StorageRoots,
}
```

- `SequenceStart`/`SequenceEnd` remain **only** for nested sequences; `SequenceEnd` for
  `main` stays as the program-output record (it is already at `[]`).
- `EntrypointStep`/`EntrypointOp` are deleted; `StepRecord` accessors
  (`output_commitment`, `input_source_commitment`, `storage_roots`,
  `is_execution_step`) get a `ProgramStart` arm. `ProgramStart` has **no input
  commitments at all** — its "input" is the outside world, authorized via the journal,
  which resolves the `checks/cfs.rs` TODO by construction.
- `TraceEvent`: delete `EntrypointBind` + `EntrypointBindEvent`; add
  `TraceEvent::ProgramStart { arguments: Vec<EntrypointArgumentBinding> }` (reuses the
  existing `EntrypointArgumentBinding` type). `SequenceStart(main)` is no longer
  published.

### 2. CFS model (`crates/raster-core/src/cfs.rs`)

- Delete `SequenceChildItem::Entrypoint`, `EntrypointItem`, `SequenceChildId::Entrypoint`
  and all their special cases in `get_sequence`, `try_get_item`, `get_child_coordinates`
  (`"__entrypoint"`).
- Add `entry_arguments: Vec<String>` to `SequenceDef` (meaningful only for `main`;
  empty elsewhere). `CfsCursor::main_entrypoint_names()` reads this field instead of
  peeking at `items[0]`.
- Add `InputBinding::EntryArgument` (alongside `Direct`/`SequenceScope`/
  `PriorItemOutput`): a consumer argument bound to one of `main`'s entry arguments. Its
  guest check: the witness's `StorageData.coordinates == CfsCoordinates([])`, with the
  selection composing from the `Field(name)` prefix exactly as today.

### 3. Compiler (`crates/raster-compiler`)

- `CfsBuilder`: stop prepending the `Entrypoint` item; set
  `SequenceDef::entry_arguments` from `main`'s parameter names.
- `FlowResolver::resolve_with_entry_arguments`: drop the `item_index_offset` hack;
  entry-argument names resolve to `InputBinding::EntryArgument` instead of
  `PriorItemOutput { index: 0 }`.
- Update the builder/resolver tests accordingly.

### 4. Runtime (`crates/raster-runtime`)

- `entry_arguments.rs`: `bind_entry_arguments(&[EntryArgumentSpec])` becomes
  `start_program(&[EntryArgumentSpec]) -> Result<EntryArgumentsBinding>`:
  1. enters `main`'s sequence scope (what `SequenceScopeGuard::enter("main")` +
     `enter_sequence_scope` do today),
  2. when specs are non-empty: reads `(encoding, commitment)` per argument from the
     manifest via the installed `SourceResolver` (unchanged logic) and calls
     `load_authorized_sources` at **coordinates `[]`** — no
     `reserve_execution_coordinates()` call, which deletes the fragile "must be the
     first write in `main`'s scope so it gets `[0]`" ordering invariant,
  3. publishes `TraceEvent::ProgramStart { arguments }` (with empty `arguments` when
     there are none),
  4. returns the `StorageRef` at `[]` for `AuthRef` construction (unchanged mechanics —
     `Field(name)` selector prefixes into the combined object).
- With the Entrypoint item gone, the first real item of `main` is index 0 again —
  `THREAD_SEQUENCE_CONTEXT` needs no offset handling.

### 5. Macros (`crates/raster-macros`)

- `entrypoint.rs` prelude: replace the `bind_entry_arguments` +
  `publish_trace_event(EntrypointBind)` pair with one `::raster::start_program(&[specs])`
  call. Emit it for **every** `main` (zero specs when no params).
- `lib.rs` `gen_sequence_wrapped_body`: for `main`, do not publish
  `TraceEvent::SequenceStart`; the scope guard / profiling stays; the `SequenceEnd`
  publish stays. Cleanest split: a `gen_main_wrapped_body` variant (`main` is already
  special-cased in the `sequence` macro).

### 6. Trace recorder (`crates/raster-runtime/src/tracing/recorder.rs`)

- Delete the `TraceEvent::EntrypointBind` arm; add a `TraceEvent::ProgramStart` arm
  that:
  1. pushes the `main` frame on the sequence callstack (as the `SequenceStart` arm does —
     `get_child_coordinates` already maps root+`main` to `[]`),
  2. when arguments are non-empty: asserts the source resolver is installed and calls
     `load_authorized_sources` at `[]` (the existing cross-process postcard panic stays
     as-is),
  3. emits `StepRecord { coordinates: [], kind: ProgramStart(...) }`.
- **Known wrinkle — witness store keyed by coordinates**: `ProgramStart` and
  `SequenceEnd(main)` now share key `[]` in `StepWitnessStore`. Two fixes required:
  - the `SequenceEnd` arm must not clobber `storage_write` (today it overwrites with
    `None`; change to only set when `Some`),
  - `storage_state_from_prefix` in `crates/raster-cli/src/commands/run.rs` iterates
    step records and applies the write found at each record's coordinates — with two
    records at `[]` it would apply the entry write twice. Gate the lookup on kinds that
    can write (`step.storage_roots().is_some()` is true only for `Exec` and
    `ProgramStart`).

### 7. Transition guest (`crates/raster-prover/guests/transition`)

- `checks/entrypoint.rs` → verify `ProgramStart`:
  - `entrypoint_coordinates()` becomes `CfsCoordinates([])`;
  - `verify_step`: `entry_arguments` must equal the CFS-declared names (from
    `SequenceDef::entry_arguments`); `output_commitment` must equal
    `combined_root(names, journal)` (unchanged function); with zero names: no write,
    roots unchanged, empty commitment.
  - `verify_genesis_authorization`: the **`Pending` state disappears**. A window either
    (a) starts at trace index 0 — its first applied step must be `ProgramStart`,
    verifiable in the same guest run (`FraudProofWindowContext::proceed` has
    `input.step_record` available), or (b) starts later — the membership witness must
    prove `[]` commits to the combined root in the window's initial storage (unchanged
    mechanism, new coordinates). Zero-args programs owe nothing (vacuously established).
  - `EntrypointAuthorization`: remove `Pending` and `assert_discharged()`/the `finalize`
    deadline check. The enum collapses to `Established | NotRequired` and never changes
    after window entry — **every verified chain state now has storage tied to the
    manifest**, which is the authorization-preservation property this proposal exists
    for.
- `checks/cfs.rs`: `verify_step_record_inputs` gets an explicit `ProgramStart` arm (no
  CFS inputs) instead of the blanket empty-coordinates early-return; the early-return
  and its TODO go away. `record_matches_item` drops the `Entrypoint` arms. New
  `InputBinding::EntryArgument` check: resolved source must be `Storage` with
  coordinates `[]`.
- `checks/io.rs` / `checks/store.rs`: mostly unchanged — `verify_storage_transition`
  already keys the write on `step_record.coordinates()`, which now matches (`[]` step,
  `[]` write).
- `get_next_expected_coordinates`: the existing `None`-item branch for `[]` already
  yields `{[], [0], ...}`, so `ProgramStart -> first item` and
  `last item -> SequenceEnd(main)` orderings hold; the "Entrypoint start" comment branch
  in `cfs.rs` gets re-derived for the new first-step shape.

### 8. CLI (`crates/raster-cli/src/commands/run.rs`)

- Replay path: `set_external_input` stays (cross-process resolver); event-name and
  coordinate display arms updated.
- `build_entrypoint_membership_witness`: coordinates `[0]` → `[]`; doc comment updated.
- Window/transition input assembly: `Entrypoint` step arms → `ProgramStart`.

## What this buys

1. **Single first event**: every trace, with or without entry arguments, begins with one
   `ProgramStart` step that both opens the program and commits its authorized inputs.
2. **Authorization is invariant**: no chain state exists between "program started" and
   "inputs authorized"; `Pending` and the discharge deadline are deleted, and the
   genesis rule becomes one sentence.
3. **Model cleanup**: the synthetic CFS `Entrypoint` item, the flow-resolver index
   offset, the vacuous unverified `SequenceStart(main)`, the `checks/cfs.rs` TODO, and
   the "bind must be `main`'s first write" ordering invariant all disappear.
4. **`main` is honestly special**: its entry is a different *kind* of step
   (journal-verified, not replay-verified) instead of a normal sequence start plus a
   bolted-on item — matching how the guest already had to treat it.

## Implementation order

1. `raster-core`: trace + CFS type changes (everything downstream is compile-driven).
2. `raster-compiler`: CfsBuilder / FlowResolver + their tests.
3. `raster-runtime`: `start_program`, storage at `[]`, recorder arm, witness-store fix.
4. `raster-macros`: main codegen (`entrypoint.rs`, `gen_sequence_wrapped_body` split).
5. Transition guest: checks + `EntrypointAuthorization` simplification (guest rebuild
   via `raster-prover/build.rs`).
6. `raster-cli`: replay/window assembly, membership witness, display arms.
7. Regenerate fixtures (hello-tiles, per project convention — guest crates live outside
   the workspace) and update integration tests
   (`crates/raster/tests/external_selection.rs`, `crates/raster-cli/tests/recur_cli.rs`,
   `guests/transition/src/tests.rs`).

## Verification

- `cargo test` across the workspace + the transition guest's own test module.
- End-to-end on the example project with `--input`/`--input-manifest`
  (`input.json` / `input_manifest.json` at repo root):
  - plain run: trace begins with a single `ProgramStart`, no `SequenceStart(main)`,
    first tile at coordinate `[0]`;
  - commit/audit path: prove a window that opens at index 0 (`ProgramStart` inside the
    window) **and** a window that opens after it (membership witness at `[]`) — the two
    routes that replaced `Pending`;
  - a zero-entry-argument program: trace still begins with `ProgramStart`, prover
    accepts.
- Negative check: tamper a step (existing `random_bit_flip.sh` workflow) and confirm the
  fraud proof still concludes, including for the `ProgramStart` step itself.

## Out of scope

- Postcard-encoded entry arguments in the cross-process recorder remain unsupported for
  commit/audit (existing documented panic), unchanged by this proposal.
