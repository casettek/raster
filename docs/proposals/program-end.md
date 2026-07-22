# Proposal: `ProgramEnd` — authorized program output as a first-class boundary step

Status: implemented (2026-07-16)
Companion to: [`program-start.md`](./program-start.md) (implemented)

> Implementation note: `ProgramEnd` carries the output binding **in the step
> record** (`ProgramEndStep::output: Option<StorageData>`) rather than in a
> bound source witness — `ProgramStart` and `ProgramEnd` share coordinates `[]`
> and thus a witness-store entry, so the output read/selection witnesses are
> built directly from the record and passed on dedicated `TransitionInput`
> fields. A `StepRecord::appends_to_storage()` predicate gates storage-write
> application to the appending step (`ProgramStart`/`Exec`), so the read-only
> `ProgramEnd` never re-applies `ProgramStart`'s entry-object write. The guest
> module was left named `checks/entrypoint.rs` (it now hosts both
> `verify_step` and `verify_program_end`); the runtime helpers live in
> `raster::{end_program_unit, end_program_output}` +
> `raster_runtime::write_program_output_artifact`.

## Problem

The ProgramStart merge gave `main` a special *entry* boundary: one step at coordinates
`[]` that starts the program with all external data loaded into storage in an
authorized way. The *exit* boundary is still the generic `SequenceEnd` inherited from
nested sequences, and it is weak:

- `SequenceEnd(main)` commits `sha256(traced output bytes)` and the guest verifies that
  commitment against a witness the prover itself supplies (`checks/io.rs`) —
  tautological: nothing ties the program's output to storage lineage or to anything a
  third party can check.
- A program has no *output artifact*: results live inside the trace/storage and cannot
  be handed to anyone, let alone fed into another program.
- The boundary pair is asymmetric: `ProgramStart` is a distinct, journal-verified step;
  the end is an ordinary sequence event.

## Goal

Introduce `StepKind::ProgramEnd` — emitted when `main` returns — that commits to an
**authorized program output**: a value that provably lives in committed storage
(produced by a verified tile), exported as an **output artifact in the same format as
external input data** (raster-encoded `output.bin` + `output.rindex` + a manifest entry
with a commitment), so one program's output can be the next program's
`--input`/`--input-manifest`.

Design decisions taken:

1. **Read-and-commit + artifact**: `ProgramEnd` references the output where it already
   lives in storage (no extra write) and the runtime additionally exports it as a
   raster-encoded artifact + `output_manifest.json`, mirroring the input side.
2. **Unit + storage-backed only**: `main() -> ()` is fine (empty output); a
   value-returning `main` must return a storage-backed `AuthRef` (a tile/`select!`
   result) — returning an inline literal as the program output is an error.
3. **Success only**: `ProgramEnd` attests only correct completion. The protocol does
   not prove failed computations — only correct output that can be transmitted to
   following programs.
4. **Output authorization as journal state**: the transition journal carries an
   `OutputAuthorization` state, the exit-side sibling of `EntrypointAuthorization`
   (with an honest asymmetry — see §7).

## Facts the design builds on (verified in code)

- `main`'s wrapped body runs as `(|| #body)()` (`auth_result_binding`,
  `crates/raster-macros/src/lib.rs`), so **every** `return` statement funnels to the
  single exit point where the end event is published — "emit on return" needs no
  control-flow analysis.
- `main`'s result is an `AuthRef<T>` (`Inline(T) | Storage(DeferredAuthStorage)`); the
  storage case resolves to `TraceStorageData { coordinates, commitment, selector,
  selection }` via `auth_ref_trace` (`crates/raster/src/input.rs`) — exactly the
  `StorageData` shape the guest already verifies for tile inputs.
- `SelectionCommitment.selected_hash` is `selection_payload_hash(selected bytes)` — the
  hash of the output value's canonical payload bytes, provable in-guest by the existing
  `verify_selection_witness` against the source object's committed root.
- Manifest entries are `{ name: { type: "sha256", encoding: "raster", commitment } }`
  (`input_manifest.json`); `write_raster_files(value, data_path, index_path) ->
  commitment` already exists (`crates/raster-runtime/src/input.rs`).
- The runtime already passes artifact paths by env (the `RASTER_TRACE_PATH` pattern in
  `crates/raster-runtime/src/tracing.rs`); tile outputs carry `RasterPayload` into the
  recorder's storage replica, so the CLI can regenerate the artifact cross-process.
- A value-returning `main` is currently a latent, unused capability (every traced
  `main` in the repo returns `()`; a generated `fn main() -> String` wouldn't even
  satisfy `std::process::Termination`). `ProgramEnd` is what makes value-returning
  `main` meaningful: the declared return type becomes the program's *output
  declaration*, not a process return value.

## Design

### 1. Trace model (`crates/raster-core/src/trace.rs`)

```rust
StepKind::ProgramEnd(ProgramEndStep)

pub struct ProgramEndStep {
    /// The storage binding of the program output: where it lives, the
    /// source object's commitment, and the selection that narrows to it.
    /// `None` when main returns unit.
    pub output: Option<StorageData>,   // reuses the existing StorageData type
    /// Commitment to the output value's canonical payload bytes:
    /// `selection.selected_hash`. Empty for unit.
    pub output_commitment: Vec<u8>,
}
```

**`ProgramEnd` attests only correct completion.** There is no error variant: a `main`
that returns `Err` (or panics) publishes **no** `ProgramEnd` — the trace ends without
its terminal step and can never be attested as a completed program execution. The
error surfaces as an ordinary process failure (non-zero exit, message on stderr),
outside the protocol.

- Recorded at coordinates `[]` as the trace's **last** step, replacing `main`'s
  `SequenceEnd` (nested sequences keep `SequenceStart`/`SequenceEnd` unchanged).
- No storage roots: it writes nothing; its reads are verified against the chain's
  current roots (like tile inputs).
- `StepRecord` accessors: `output_commitment()` → `Some`;
  `input_commitment()`/`input_source_commitment()` → `None`; `storage_roots()` →
  `None`; `is_execution_step()` → `true` (verified via storage/selection proofs, not a
  byte comparison).
- `TraceEvent::ProgramEnd(ProgramEndEvent { output: Option<…> })` replaces
  `SequenceEnd(FnCallRecord)` for `main`.

### 2. CFS (`crates/raster-core/src/cfs.rs`, `crates/raster-compiler`)

- `SequenceDef` gains `#[serde(default)] produces_output: bool` (sibling of
  `entry_arguments`; meaningful only for `main`). `CfsBuilder` sets it from `main`'s
  return type (`seq.function.output`); unit and `Result<()>` → `false`.
- `CfsCursor::main_produces_output()` beside `main_entrypoint_names()`.
- The guest holds `ProgramEnd` to it: output binding present **iff** declared.

### 3. Runtime (`crates/raster-runtime`)

New `end_program(...)`, symmetric to `start_program`:

1. Accepts `main`'s auth result (unit / `AuthRef<T>` / `Result<AuthRef<T>>`). For a
   fallible `main` that returned `Err`: publish nothing, export nothing — return the
   error so the generated `fn main()` exits non-zero. Only the `Ok` path continues.
2. **Storage-backed enforcement**: for a value-returning `main`, `AuthRef::Inline` is a
   runtime error ("program output must be a tile or select! result").
3. Resolves the binding via the existing `auth_ref_trace` machinery → `StorageData` +
   `selected_hash`.
4. **Artifact export** (when `RASTER_OUTPUT_DIR` env is set — set by `cargo raster run`
   to the run-artifacts dir; plain `cargo run` skips file emission):
   - materialize the value, `write_raster_files(&value, output.bin, output.rindex)`,
   - write `output_manifest.json`: `{ "output": { type: "sha256", encoding: "raster",
     commitment: <structural root from write_raster_files> } }` — byte-for-byte the
     input-manifest entry format, so the artifact can be the next program's
     `--input`/`--input-manifest`.
5. Publishes `TraceEvent::ProgramEnd`.

Exit-code semantics unchanged: generated `fn main()` returns `()` (or propagates
`Result<(), E>` for fallible-unit); a value-returning `main`'s value goes to the
artifact, not the process return.

### 4. Macros (`crates/raster-macros`)

- `gen_sequence_wrapped_body` for `main`: replace the `SequenceEnd` publish with one
  `::raster::end_program(__raster_result)` call at the single exit point (all `return`s
  already funnel there via the body closure). Publishing lives inside `end_program`,
  mirroring how `start_program` owns the start event.
- `materialize_main_result`: `Value(T)` no longer materializes into the process return
  — generated `fn main()` returns `()`; `Unit`/`Fallible(())` keep today's signatures.
- Module rename: `entrypoint.rs` → `program.rs` (it now generates both boundaries).

### 5. Trace recorder (`crates/raster-runtime/src/tracing/recorder.rs`)

`TraceEvent::ProgramEnd` arm (replacing the `SequenceEnd` path for `main`):

- pops `main`'s frame (asserts no active recur streams, as `SequenceEnd` does),
- emits `StepRecord { coordinates: [], kind: ProgramEnd(...) }`,
- **cross-check**: rebuilds the claimed selection from its own storage replica via the
  existing `storage_selection_witness` and asserts the recomputed `selected_hash`
  matches the event's — the cross-process replay independently re-derives the output
  commitment instead of trusting the user process,
- witness-store: updates the `[]` entry (shared with `ProgramStart`) without clobbering
  `storage_write` — the guard added in the ProgramStart work already covers this.

### 6. Transition guest (`crates/raster-prover/guests/transition`)

Rename `checks/entrypoint.rs` → `checks/program.rs` (it now verifies both boundaries)
and add `verify_program_end`:

- coordinates must be `[]`; output binding present iff `main_produces_output()`;
- verify the storage read: the source object at `output.coordinates` commits to
  `output.commitment` in the **current** storage roots — reuse
  `verify_storage_read_witness` exactly as tile inputs do;
- verify the selection: `verify_selection_witness(output.selection, witness)` and
  `output.commitment == selection.source_root_hash`;
- `output_commitment == selection.selected_hash`;
- storage roots unchanged (no write);
- **terminal step**: `get_next_expected_coordinates` returns an empty set after
  `ProgramEnd` — nothing may follow it in a valid trace (this also removes the current
  looseness where post-`SequenceEnd(main)` expectations were never pinned down).

Witness plumbing: the `ProgramEnd` step's read + selection witnesses flow through the
existing per-step `storage_witness`/`storage_selection_witnesses` channels — the shapes
are identical to tile-input witnesses, and the CLI already builds them per step.

### 7. `OutputAuthorization` (`crates/raster-core/src/transition.rs`, guest `fraud_proof.rs`)

The journal gains `output_authorization: OutputAuthorization`:

```rust
pub enum OutputAuthorization {
    /// CFS declares main returns no value — nothing to authorize.
    NotRequired,
    /// The chain has not reached the program's end yet.
    Pending,
    /// A ProgramEnd step was verified inside this chain: the committed
    /// output provably lives in committed storage.
    Established,
}
```

- Genesis: `NotRequired` iff the CFS says no output, else `Pending`. No
  membership-witness route is needed (unlike ProgramStart): `ProgramEnd` is the trace's
  *last* step, so no window can open "after" it — `Pending` mid-trace is the honest
  state, not a debt.
- `Established` when `verify_program_end` passes in-window; inherited across `Next`
  journals.
- **Honesty note (deliberate asymmetry with `EntrypointAuthorization`):** a fraud chain
  concluding at a mid-trace divergence legitimately never sees `ProgramEnd`, so the
  guest cannot demand `Established` at `Finished`. The invariant is enforced where it
  is meaningful:
  1. the guest makes `ProgramEnd` the unique terminal step, so any committed full trace
     necessarily contains it and the output commitment is bound into the fingerprint —
     forging an output means diverging from the fingerprint, which is fraud-provable;
  2. host-side full-trace verification (`TraceVerifier`, the `commands/run.rs` audit
     path) asserts the trace ends with a verified `ProgramEnd` and that the artifact
     commitment matches;
  3. any consumer accepting a "program completed" journal requires `Established`.

### 8. CLI (`crates/raster-cli/src/commands/run.rs`)

- `run`: set `RASTER_OUTPUT_DIR` to the run-artifacts dir; after the run, print the
  artifact paths + commitment. The replay recorder performs the §5 cross-check; the CLI
  can additionally regenerate `output.bin`/`output.rindex` from its storage replica
  (tile outputs carry `RasterPayload`), so `--commit`/`--audit` reproduce the artifact
  without trusting the user process's files.
- Step display: a `program_end_coordinates` label; fraud-corruption candidates
  unchanged (a `ProgramEnd` has no replayed output — though corrupting its
  `output_commitment` is a good negative test).

## Resulting trace shape

```
ProgramStart  []      inputs authorized in   (manifest -> storage)
  ...tiles/sequences at [0..n]...
ProgramEnd    []      output authorized out  (storage -> artifact + manifest)
```

`main` is now a fully special sequence: both boundaries are journal/storage-verified
program steps, symmetric in shape and in artifact format — input manifest in, output
manifest out. Programs compose: one program's `output_manifest.json` + `output.bin` is
the next program's `--input-manifest` + `--input`.

## Alternatives considered

- **Canonical-write** (write the output to a reserved coordinate, fully symmetric to
  `ProgramStart` writing at `[]`): rejected — it duplicates an already-committed
  object, needs a new reserved-coordinate scheme, and read-and-commit proves the same
  fact with machinery that already exists (`verify_storage_read_witness` +
  `verify_selection_witness`).
- **Commit the re-encoded structural root in-guest** instead of `selected_hash`:
  rejected — it would require Merkleizing the value inside the riscv32 guest;
  `selected_hash` is already proven by the selection witness, and the artifact's
  structural-root manifest commitment is publicly recomputable from the artifact bytes
  by anyone.
- **Strict `Finished`-deadline for `OutputAuthorization`** (mirror of the removed
  `EntrypointAuthorization::Pending` discharge): rejected as unsound — fraud proofs
  legitimately conclude mid-trace, before any output exists (see §7).
- **Recording program errors as a `ProgramEnd` variant**: rejected — the protocol
  attests correct output for transmission to following programs; a failed run is simply
  an unattested (incomplete) trace, exactly like a panic.

## Implementation order

1. `raster-core`: `ProgramEndStep`/`StepKind`/`TraceEvent`,
   `SequenceDef::produces_output`, `OutputAuthorization` (+ journal field).
2. `raster-compiler`: `CfsBuilder` sets `produces_output`; tests.
3. `raster-runtime`: `end_program` + `RASTER_OUTPUT_DIR` export; recorder `ProgramEnd`
   arm with the cross-check.
4. `raster-macros`: main exit-point rewrite (`end_program`),
   `materialize_main_result` adjustment; module rename `entrypoint.rs` → `program.rs`.
5. Transition guest: `checks/program.rs` (`verify_program_end`), terminal
   next-coordinates, `OutputAuthorization` wiring in `fraud_proof.rs`/`main.rs`.
6. `raster-cli`: env, artifact print/regeneration, display arms, witness assembly.
7. hello-tiles: give `main` a return value (e.g. the final greeting) to exercise the
   whole path; update `recur_cli` assertions; regenerate fixtures.

## Verification

- Unit: guest tests for `verify_program_end` (accept / tampered commitment / binding
  present-vs-declared mismatches / terminal coordinates); recorder cross-check test;
  `end_program` inline-rejection test.
- End-to-end (hello-tiles with a value-returning `main`):
  - `cargo raster run ...` → trace ends `ProgramEnd []`;
    `output.bin`/`output.rindex`/`output_manifest.json` produced; recomputing the
    raster commitment of `output.bin` matches the manifest, and
    `selection.selected_hash` matches the recorded step;
  - **round-trip**: feed the produced artifact back as another run's
    `--input`/`--input-manifest` (output format == input format);
  - `--commit`/`--audit` with a window covering `ProgramEnd` → journal
    `output_authorization: Established`; a mid-trace window stays `Pending`;
  - negative: corrupt `ProgramEnd.output_commitment` → fraud proof concludes.
- `main() -> ()` program still runs; `ProgramEnd` with no output verifies as
  `NotRequired`.
- Fallible `main` returning `Err`: process exits non-zero, trace ends **without** a
  `ProgramEnd`, and the audit/commit path refuses to attest it as a completed program.

## Out of scope

- Multiple named outputs (a single `output` artifact for now; the manifest format
  leaves room to grow).
- Postcard-encoded output artifacts (raster only, mirroring the input-side
  cross-process constraint).
