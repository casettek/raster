# Issue: `fraud-evidence-storage-unavailable` — a detected divergence cannot be proven, because evidence construction needs a storage object no run ever wrote

Status: **partly fixed** 2026-09-17. The coordinate divergence in §2b is fixed and the reported
panic no longer occurs; the fraud proof still does not complete, for unrelated reasons named in
§7. Unowned.

Reproducible against `feature/recur-mid-seed` at `c484ef7`, on `raster-inference`'s
`prompt-prepare` (1,997 steps, `--fraud-proof-window-size 32`, `commit.bin` 10,852 bytes). Every
code citation is committed code; the run results in §2 are from that reproduction.

Fraud is **detected**. The CLI then panics building the receipt, so the verdict is a crash rather
than a fraud proof. In an optimistic protocol where silence is acceptance, that settles
identically to having detected nothing.

Related:

- [`fraud-window-trace-ends`](./fraud-window-trace-ends.md) (closed 2026-09-16) — §2 records a
  panic of the **same class** at a different site: `witness_record_inputs` failing to resolve a
  source record, so a detected divergence produced no window. That one is fixed (`0a71d10`), and
  it ran *earlier* in the same path — inside `verify()`, before `prove()` is reached. Whether
  fixing it is what exposed this is unresolved and cheaply testable (§5).
- [`selection-unbound-from-execution`](./selection-unbound-from-execution.md) — adjacent, and the
  opposite polarity. That is a soundness gap (a fabricated recur sweep is *accepted*); this is
  fail-closed (an honest detection cannot be *proven*). Both land on recur iterations and their
  storage selections, so they may share a root cause; neither subsumes the other.
- [`chain-io-commitment`](../proposals/chain-io-commitment.md) §3 — owns the dispute path a fraud
  proof feeds. It assumes throughout that a detected divergence yields a constructible proof.
  `trace-end-windows` closed the two window-geometry cases where that failed; this is a third,
  and it is not about window geometry.
- [`trace-leaf-field-binding`](../proposals/trace-leaf-field-binding.md) §"A prerequisite that was
  its own defect" — the precedent for how this class hides: a panic in host-side evidence
  construction, `panic!` inside an `unwrap_or_else`, reached only on the fraud path and therefore
  never exercised by a passing test.

## 1. The defect

`prove()` builds, for each step of the fraud window, the witnesses the transition guest needs.
One of them is the storage selection witness for each of the step's storage-bound inputs:

```rust
// crates/raster-cli/src/commands/run.rs:830, :851
for step_record in &fraud_window.items {
    ...
    let storage_selection_witnesses =
        build_storage_selection_witnesses(input_source_witness.as_ref(), trace_recorder);
```

`build_storage_selection_witnesses` (`:737`) iterates the step's recorded
`FnInput.storage` map, skips any binding whose selection is empty (`:751-753`), and asks the
recorder to produce a witness for the rest:

```rust
// :761-771
.storage_selection_witness(&reference, &storage.selector, storage.selection.payload_kind)
.unwrap_or_else(|error| {
    panic!("Failed to build storage selection witness for '{}': {}", binding_name, error)
})
```

That resolves to `StorageManager::selection_witness`
(`crates/raster-runtime/src/storage.rs:599`), whose first act is
`self.verify_reference(reference)?` (`:605`), which is a lookup in the **auditing process's own**
live storage map:

```rust
// crates/raster-runtime/src/storage.rs:559-563
fn verify_reference(&self, reference: &StorageRef) -> Result<&StoredObject> {
    let stored = self.objects.get(&reference.coordinates).ok_or_else(|| {
        Error::Other(format!("Missing storage object at coordinates {:?}", reference.coordinates))
```

When that lookup misses, the `Err` reaches `run.rs:767` and becomes a panic.

### The map is not lossy, which narrows the cause

`objects` is a `BTreeMap` (`storage.rs:165`) with exactly **one** insert site (`:464`), no
`remove` and no `retain` anywhere in the file, and a `!contains_key` assertion guarding against a
second write to the same coordinate (`:439-443`). Storage is write-once and insert-only.

So the auditing process did not *drop* the object. It never wrote one. And since the byte-100
experiment tampers only the commitment file — not the program, not the inputs — the auditing run
executes exactly what the committing run executed, so the committing run did not write one
either.

**The trace records an input binding whose source coordinates no execution ever backs with a
storage object.** Evidence construction is the first thing that asks for it.

The code already half-anticipates this, without handling it:

```rust
// run.rs:757-760
// The recorded commitment says which view of the node it
// committed to. This process did not produce the trace, so
// it cannot infer that from the payload — it has none yet.
```

## 2. Reproduction

Four experiments, all `^= 0x01` against the honest regenerated `commit.bin`:

| target | byte | result | exit |
| --- | --- | --- | --- |
| fingerprint header | 0 | `Invalid commitment: Fingerprint claims 1997 items of 5 bits (157 packed blocks) but holds 125 blocks` | 1 |
| packed fingerprint bits | 100 | **fraud detected, then panic in `prove()`** | 101 |
| `revealed_items` | 5000 | `Verification Success` — accepted | 0 |
| built-in injector (`--commit fraud_demo.bin`) | — | **fraud detected, then panic in `prove()`** | 101 |

```
thread 'main' panicked at crates/raster-cli/src/commands/run.rs:766:25:
Failed to build storage selection witness for 'step': Missing storage object
at coordinates CfsCoordinates([5, 0, 56])
```

### Why byte 100 is detection and byte 0 is not

`prove()` has exactly one call site — `run.rs:323`, inside the `VerificationResult::Fraud` arm —
and the panic is inside `prove()` (`:851` → `:767`). Reaching it requires `verify()` to have
returned `Fraud`, i.e. a comparison against re-execution.

Byte 0 is a different kind of failure: `TraceCommitment::validate` rejecting a self-inconsistent
header before any re-execution is compared. `fraud-window-trace-ends` §6 already warns that this
reads like detection and is not.

### Not an artifact of hand-editing

The supported path reproduces it identically. `--commit fraud_demo.bin` runs `fraud()`
(`run.rs:528`), which corrupts the `output_commitment` of one **randomly chosen** executed step
(`:537-545`) before the commitment is built — an internally coherent cheat, not a mangled file.
Same binding (`'step'`), same coordinates (`[5, 0, 56]`), ~17.9 s.

### Named 2026-09-16: the source coordinate points at a step that never existed

A temporary diagnostic (§5) reporting every window step's bindings instead of panicking on the
first gives one failure, not many:

```text
[diag] window[00] step [5, 0, 11]     binding "input"              source [4, 0]     -> ok
[diag] window[00] step [5, 0, 11]     binding "merge_bucket_count" source []         -> ok
[diag] window[00] step [5, 0, 11]     binding "merge_buckets"      source []         -> ok
[diag] window[01] step [5, 0, 11, 0]  binding "piece"              source [4, 0]     -> ok
[diag] window[02] step [5, 0, 11, 1]  binding "bucket_count"       source []         -> ok
[diag] window[02] step [5, 0, 11, 1]  binding "step"               source [5, 0, 56] -> FAILED
        | source in trace: false
        | sibling indices under that prefix: first=0 last=14 count=30
```

So neither hypothesis was right. It is **one binding on one step**, and the reason is not that
storage failed to retain the object:

- **`[5, 0, 56]` does not appear in the trace at all.** No step ever executed there, so nothing
  could have written it. Storage is insert-only with a duplicate-write assert
  (`storage.rs:439-443`, `:464`), so this is not retention.
- **The sweep at `[5, 0]` ran iterations 0-14**, 30 trace records (a `Start` and an `End` each).
  Iteration 56 is far past the end.
- **The coordinate has the wrong arity for its position.** `step` is a tile argument of type
  `MergeStep` (`prompt-prepare/src/lib.rs:64`), produced by a sibling tile inside the same
  iteration body — so its source should be a length-4 sibling coordinate such as
  `[5, 0, 11, 0]`, which is exactly the shape the neighbouring `piece` binding resolves against.
  It records a length-3 coordinate instead.

**This is a defect in the honest recording path, not in fraud handling.** The audit re-executes the
program itself; the tamper is confined to the commitment file. The binding above was produced by
the auditing process's own honest run. The fraud path is merely the first consumer that asks for
the object, which is why it surfaces here and nowhere else.

Unverified, offered for whoever picks this up: the arithmetic of a flattened index fits. If an
iteration body holds 5 items, `11 * 5 + 0 + 1 == 56`, which would make `[5, 0, 11, 0]` and
`[5, 0, 56]` the same step addressed two ways — nested, and flattened into the site. That is a
guess from one data point, not a claim.

## 2b. Root cause: two storage maps keyed by two different coordinate schemes

### The schemes

`SequenceExecutionContext::reserve_execution_coordinates` (`storage.rs:826-842`) tests
`recur_stack` before the sequence stack:

```rust
if let Some(recur_frame) = self.recur_stack.last_mut() {
    coordinates.push(recur_frame.next_iteration_index);
    recur_frame.next_iteration_index += 1;   // advanced per tile, not per iteration
    return Ok(coordinates);
}
let frame = self.stack.last_mut()...          // the iteration frame — unreachable
```

`enter_recur_sequence_iteration` (`:785-798`) pushes a correct `SequenceFrame` at
`site ++ [iteration]` with `next_child_index: 0`, but the recur branch wins first. So inside a
recur-**sequence** body the running program addresses storage as `site ++ [flat]`, with the
counter advanced by every tile, while the **trace recorder** addresses the same executions as
`site ++ [iteration, item]`.

Measured directly — every length-3 write under `[5, 0]`, in order:

```text
1 2 4   6 7 9   11 12 14   16 17 19   21 22 24   26 27 29   31 32 34   36 37 39
41 42 44   46 47 49   51 52 54   56 57 59   61 62 64   66 67 69   71 72 74
```

45 writes, **15 groups of three** — one per iteration, at `5k+1, 5k+2, 5k+4`. Iteration 11 writes
`56, 57, 59`, and `[5, 0, 56]` is its first: `begin_merge_step`, the tile that produces the
`step` binding which fails.

### Why the lookup misses anyway

`[5, 0, 56]` *is* written — into `THREAD_STORAGE`, the executing program's map. The fraud path
asks a different one. `TraceRecorder::new` builds `storage: StorageManager::new()`
(`tracing/recorder.rs:311`), its own instance, keyed by the recorder's nested coordinates. So:

| | writes | keyed by |
|---|---|---|
| `THREAD_STORAGE` | the running program | `[5, 0, 56]` — flat |
| `TraceRecorder.storage` | the recorder | `[5, 0, 11, 0]` — nested |

A value carries the `StorageRef` the runtime gave it, so the recorded binding says `[5, 0, 56]`.
`build_storage_selection_witnesses` resolves it against the **recorder's** map, which has never
heard of that key. Both maps are internally consistent; the fraud path is the first thing that
crosses between them, which is why nothing else notices.

The nested scheme is the correct one — it is what the CFS, the trace and the guest's coordinate
checks all use. The flat one is the anomaly.

### Recorded because the measurement lied once

This root cause was proposed, **wrongly disproven**, then re-confirmed. The disproof dumped
`TraceRecorder.storage` — the nested map — and concluded no flat coordinates existed. Two mistakes
compounded: the wrong map, and output truncated at 80 keys sorted as *strings*, where
`"[5, 0, 1]"` sorts after every `"[5, 0, 1X, …]"`, so the length-3 keys fell off the end twice
over. The histogram printed alongside said `{3: 131}` and was read past.

Worth keeping: a dump that only samples, and sorts structured data as text, is capable of
producing a clean and entirely wrong negative.

## 2c. The fix, and what it did and did not resolve

`RecurFrame` gains `iteration_open`, set by `enter_recur_sequence_iteration` and cleared by
`exit_recur_sequence_iteration`. `reserve_execution_coordinates` takes the recur branch only when
it is *closed*:

```rust
if let Some(recur_frame) = self
    .recur_stack
    .last_mut()
    .filter(|frame| !frame.iteration_open)
{ ... }
```

A recur **tile**'s iteration is one tile execution and pushes no `SequenceFrame`, so the recur
branch stays correct there. Inside a recur **sequence** iteration the body's frame is already on
`self.stack` at `site ++ [iteration]`, so falling through assigns `site ++ [iteration, item]` —
what the recorder and the CFS both use. Clearing the flag also stops body steps advancing
`next_iteration_index`, which is what made the iteration numbering drift.

**The nested scheme is authoritative, and the guest is what settles it.** `verify_one_binding`'s
`PriorItemOutput` arm builds `source_coordinates = parent ++ [intra_sequence_item_index]` and, for
a tile source, asserts the recorded `storage_meta.coordinates` equals it
(`guests/transition/src/checks/cfs.rs`). A flat coordinate could never satisfy that — so recur
sequences were not provable at all, matching `recur-state-chaining`'s note in
`docs/proposals/README.md` that the same class of defect went unnoticed because
`verify_step_record_inputs` runs only on the fraud path.

**Verified:** `prompt-prepare` regenerated and audited honestly (`Verification Success`), then
audited against a byte-100 tamper. The run now reaches
`"Replaying transition frontier with transition guest..."` (`run.rs:1023`), i.e. **past** the
witness loop at `:830-851` that used to panic. `raster-core` 141, `raster-runtime` 62 and
`raster-prover` `trace::tests` 28 all pass; clippy clean.

**Cost:** every storage coordinate inside a recur-sequence body moves, so recorded traces and any
commitment over them change. `prompt-prepare/commit.bin` must be regenerated.

## 7. Still blocking a completed fraud proof

Neither is this issue's defect; both were uncovered by running past it, and each deserves its own
issue if it reproduces outside this environment.

- ~~**Program identity drift**~~ — **fixed 2026-09-17.** The two paths never disagreed. Plain
  `cargo raster build` defaults to `--backend native`, and `commands::build` emits `program.bin` +
  `Raster.lock` **only** under risc0, because image ids need compiled guests — so the command the
  error recommends reports success, changes nothing, and returns the same error. `cargo raster
  build --backend risc0` writes `3489fc70…`, exactly what `reassemble_and_verify` computes, and
  the drift clears. Fixed by naming the working command in all three messages
  (`program.rs:279`, `:308`, `:326`) and by having `build` say when it has *not* written those
  artifacts rather than printing a bare `Build complete!`. Only the fraud path reaches the check:
  an honest audit returns `Ok` before `prove()`, which is why a stale lock can sit unnoticed.
- ~~**The transition guest cannot decode the `ProgramDefinition` frame**~~ — **fixed 2026-09-17.**
  Not a stale ELF: `skip_serializing_if` on four `cfs.rs` fields omitted them on write while
  `Deserialize` still read at those offsets, shifting every later field. postcard is positional and
  non-self-describing, so the attributes are unusable there. Fixed by removing them and keeping
  `#[serde(default)]`.
- ~~**The entry-argument binding can never be recomputed**~~ — **fixed 2026-09-17.**
  `checks/store.rs:58`, `"Storage read witness commitment does not match requested commitment"`,
  reached by every fraud window that opens *after* `ProgramStart` in a program with entry
  arguments. Not corruption: the two roots are the same two commitments in different encodings.
  `AuthorizationJournal.external_inputs_commitments` holds the manifest's **lowercase hex text** —
  `normalize_hash_string` in the authorization guest ends in `String::into_bytes`, so a sha256
  entry is 64 ASCII bytes, not a digest — while the entry object the runtime builds
  (`backing::ReferencedObject::combined_root`) hashes raw `source.commitment` bytes.
  `checks::entrypoint::combined_root` fed the text straight to `struct_commitments_root`, hashing
  the digest's *spelling*.

  Confirmed arithmetically before changing anything: over the same pair
  `(tokenizer, initial_pieces)`, hashing the decoded digests reproduces the storage object at `[]`
  (`228df060…`) exactly, and hashing the text reproduces what the guest computed (`1dea6b5a…`).

  Fixed in `combined_root`, not in the authorization guest: the hex-text form is the journal's
  established contract — `chain_fraud/src/main.rs:136` compares against it with `hex_lower`, and
  `transition.rs:558` pins it as a byte-string literal — so changing the guest would move the
  authorization image id *and* break the chain-fraud check. The decode is strict (64 lowercase hex
  bytes, nothing else): accepting both spellings would let two distinct roots authorize one
  manifest entry, and a prover would present whichever its claimed storage state matched.

  **Why no test caught it.** Every entrypoint test fed `combined_root`'s own output back as the
  expected value — tautologies that hold under any encoding. The one test pinning an external
  convention, `combined_root_matches_struct_hash_convention_over_declared_commitments`, built its
  journal from `sha(b"…")` raw digests, a shape the real authorization guest cannot produce. The
  fixtures now hex-encode, which makes that test non-tautological on its own.

  **Verified:** `prompt-prepare` audited against `fraud_demo.bin` now runs the transition guest for
  ~20 minutes at ~1900% CPU before failing elsewhere, where every previous attempt asserted in
  seconds. Guest tests 84, `raster-core` 141, clippy clean.

- ~~**The transition guest exhausts its heap**~~ — **fixed 2026-09-17.** Newly reached once the
  binding recomputed:

  ```
  transition.rs → Guest panicked: Out of memory! You have been using the default bump
  allocator which does not reclaim memory. Enable the `heap-embedded-alloc` feature...
  ```

  **The error's own advice is wrong here, and measuring is what showed it.** Two facts rule it out.
  The guest runs **once per step**, not once per window (`prove_transition_window` builds a fresh
  `ExecutorEnv` and calls `prover.prove` inside the step loop), so nothing accumulates across a
  window and there are no per-step temporaries to reclaim. And the failing step's input is **106 MB
  of live data**, which a reclaiming allocator cannot shrink — the risc0 guest address space is
  smaller than the working set it is being handed.

  Measured on `prompt-prepare` in `RISC0_DEV_MODE=1` (full guest execution, no proving — minutes
  instead of hours), printing each step's serialized guest input:

  | step | whole input | largest field |
  | --- | --- | --- |
  | `[2, 0, 14, 4]` | 70,848 | `entrypoint_membership_witness` 9,578 |
  | `[2, 0, 14]` | **106,091,826** | `storage_selection_witnesses` **106,038,269** |

  All of it is one entry: selection `merge_buckets` = **106,037,710 bytes**. Its siblings on the
  same step are `merge_bucket_count` 306 and `input` 213. Executed alone, the first step completes
  in 28,412,563 cycles over 33 segments with no OOM, so the first step was never the problem —
  it is simply slow to prove (>22 min), which is what made proving useless as a measurement tool.

  **Cause.** `merge_buckets: List<MergeBucket>` is the tokenizer's whole merge table, passed as a
  *whole-object* argument to the sequence at `[2, 0, 14]`. `build_storage_selection_witnesses`
  (`run.rs:737`) builds a witness for every storage binding with `selected_len > 0`, including one
  whose selector is empty — so the witness's `bytes` field is the entire object. The guest then
  runs `verify_selection_witness`, which hashes all of it, to re-derive a root it already holds:
  `storage_meta.commitment` is asserted equal to `selection.source_root_hash` two lines earlier,
  and `verify_storage_read_witness` has already proven that commitment sits at those coordinates in
  the authenticated store.

  For a whole-object binding the fold therefore establishes nothing the guest does not already
  know, at the cost of moving and rehashing the entire value — which is precisely what an
  `AuthRef` exists to avoid.

  **The fix: a forwarded reference ships its root, not its value.**

  A sequence — ordinary or recur — receives storage arguments as references and forwards them
  inward; only a tile that consumes a value ever sees bytes. The witness builder did not make that
  distinction, so it materialized the value for every binding. Worse, `SequenceStart` carries no
  storage roots, so `verify_storage_transition` returns before the selection branch — the 106 MB
  was deserialized into the guest and then *never looked at*.

  `SelectionWitness` gains `selected_root: Option<Hash32>`, set instead of `bytes`, and
  `verify_selection_reference` folds from that root to `proof.root_hash`. Sound because the fold is
  self-authenticating: `root_hash` is checked against a commitment already proven to sit at the
  binding's coordinates, and a wrong root cannot reach it without a collision. That establishes
  exactly the scoping fact a forwarded reference needs — *this reference resolves inside an object
  the store authenticates* — and nothing about bytes the step never read.

  The rule lives once, in `raster_core::trace::binding_requires_payload`, and both the host builder
  and the guest call it, so the host cannot ship a shape the guest rejects nor choose a weaker one:

  | condition | payload required | why |
  | --- | --- | --- |
  | `StepKind::Exec` | yes | a tile ran on these bytes |
  | binding is `"input"` | yes | `authenticated_source_len` reads the sweep bound `L`; the list *metadata* view, so small |
  | binding cited as a `BoundIndex` source | yes | `verify_bound_index_bindings` compares its `selected_hash` against the claimed index, which means nothing unless bytes back it |
  | otherwise | no | forwarded reference |

  The third row is the subtle one: without it a prover could forward an index source as a
  reference, forge its `selected_hash`, and have any index accepted.

  The shape check runs at the top of `verify_storage_transition`, *before* the storage-roots guard,
  because a `SequenceStart` exits that guard immediately and is exactly where forwarded bindings
  live. `SelectionProof` is host-supplied evidence and is not in the recorded trace, so **no trace
  leaf, fingerprint or `program_commitment` moves** — only the transition guest image id, which
  this cycle already moves.

  **Verified:** the audit that OOM'd now proves past that step — zero `Out of memory` against one
  before — and stops on an unrelated pre-existing check (below). `raster-core` 150,
  `raster-runtime` 62, transition guest 88, clippy clean.

  **Not fixed by this:** a tile that genuinely consumes a value larger than the guest heap. That
  payload is the thing being proved, so it cannot be reduced to a root; it is what
  `docs/proposals/paged-bytes.md` addresses.

- ~~**A `SequenceEnd` step is handed an input source witness**~~ — **fixed 2026-09-17.**
  `checks/io.rs` → `SequenceEnd must not carry input source witness`. `StepWitnessStore` is keyed
  by coordinates alone and a sequence's `SequenceStart`/`SequenceEnd` share them: the End does
  `get_mut` on the Start's entry and fills in `output_data` only. The recorder already guards this
  sharing for `storage_write` (`recorder.rs`: *"`main`'s `SequenceEnd` shares coordinates `[]` with
  the `ProgramStart` step"*) and left `input_source_witness` and `input_data` inherited.

  It stayed hidden because `TraceEvent::ProgramStart` inserts `input_source_witness: None`, so
  *main*'s `SequenceEnd` at `[]` reads `None` and passes. Only a **nested** sequence's end inherits
  a real witness, and only a fraud window inside one reaches it.

  The naive host-side gate would have broken plain nested sequences: `checks/cfs.rs`'s
  `verify_step_record_inputs` returns early for root coordinates and recur iterations but not for a
  plain nested `SequenceEnd`, which falls through to `input_source_witness.unwrap_or_else(panic)`.
  So the guest held two contradictory expectations. `cfs.rs` was the wrong one, and its check was
  **vacuous**: `input_source_commitment` is `None` for a `SequenceEnd`, so nothing tied that witness
  to the record and anything it "proved" could have been fabricated. Fixed by returning early there
  — after `record_matches_item`, so the step is still held to its CFS item — and by having the host
  take each witness only when the record declares the matching commitment. The `io.rs` message now
  names the real condition; it said "SequenceEnd" while covering `ProgramStart` and `ProgramEnd`
  too.

  **A second finding from the same measurement.** With `[2, 0, 14]` reduced, the 106 MB reappeared
  at `[2, 0]` — the recur *site* step, whose kind is `Exec` and which `binding_requires_payload`
  therefore treated as a consumer. It is not: the recorder emits `ExecTarget::RecurTile` /
  `RecurSequence` only on the step that **closes** a site, which executes nothing and merely holds
  the site's arguments, while `ExecTarget::Tile` marks both `TileExec` and
  `RecurTileIterationExec` — every step where a tile really ran. The rule now keys on the target,
  which is the same predicate as `StepRecord::requires_replay_proof` and for the same reason.

  **The fixtures described a trace that cannot exist.** `recorder.rs`'s tests recorded
  `TraceEvent::SequenceStart` and `SequenceEnd` with `fn_name: "main"` — events the generated code
  never emits. `#[sequence] fn main` routes to `gen_main_wrapped_body`, not to
  `gen_sequence_wrapped_body` (`raster-macros/src/lib.rs`, `if item_fn.sig.ident == "main"`), and
  that generator publishes **no sequence events at all**: main's boundaries are `ProgramStart` and
  `ProgramEnd`, symmetrically. Exactly two steps sit at `[]`.

  The `is_main` branch inside `gen_sequence_wrapped_body` is dead code — that function is only
  reached on the non-main arm — and reading it as "main takes this path with the start suppressed"
  is what produced a wrong account of the root shape twice while diagnosing this. The fixture therefore took the
  wrong arm of both `record` and `StepWitnessStore::insert`, and the tests neither exercised the
  real root shape nor documented it — which is what made this bug hard to reason about and led to
  one wrong explanation of it. The same pattern as the authorization-journal fixtures in the
  entry-argument blocker above: a test that builds an input the producer cannot produce.

  Fixed: `start_main` now records `ProgramStart` with zero arguments (the supported "program still
  starts, binding nothing" path), and three tests pin the real shape — `ProgramStart` creates the
  root entry with no input source; main's `SequenceEnd` lands on it and adds none; a nested
  sequence's `SequenceEnd` lands on its own `Start`'s entry and inherits its input source, which is
  exactly why witnesses must be selected by the record's declarations rather than by coordinates.
  The "Missing step witness entry" panic also named only `SequenceStart`, which is wrong at the
  root; it now names both creators.

  **Verified:** the audit now proves **5** transition steps with **0** `Out of memory`, against 2
  and 1 before. `raster-core` 152, `raster-runtime` 65, transition guest 90, clippy clean.

- ~~**A `SequenceStart` step is handed a storage witness**~~ — **fixed 2026-09-18.**
  `checks/store.rs` → `Only execution steps may carry storage witnesses`. `run.rs` built the read
  witnesses from the step's storage bindings gated only on the witness existing, never on the step
  kind — and a `SequenceStart` legitimately has storage bindings, since that is how a sequence
  receives its arguments. The write side one block below was already gated
  (`if step_record.appends_to_storage()`), with a comment describing the same class of bug; this
  was its read half.

  Fixed by gating the loop on `step_record.storage_roots().is_some()`, placed on the loop rather
  than on the `StorageWitness` that wraps it so the proofs are never built at all — each binding
  costs a coordinate-index membership proof and a log witness whose only consumer is the discarded
  witness. (An earlier draft justified the placement by claiming `membership_proof` would panic on
  an absent coordinate; the failing run disproves that — the guest's rejection is proof the host
  built the witness successfully, so every coordinate resolved.)

  **Verified:** the audit now proves **6** transition steps, 0 `Out of memory`.

- **Why a `SequenceStart` needs no storage evidence at all** (recorded because an earlier draft of
  this file claimed the opposite). A step with no `StorageRoots` never proposes a root: the guest
  returns its own carried frontier unchanged, and every step that *does* declare roots must match
  it (`Execution-step storage root before does not match current storage root`). So a sequence step
  is transparent to the storage chain — it cannot alter the root because it is never asked for one,
  and giving it roots would be *worse*, handing a prover somewhere to propose one.

  Its arguments are authenticated in three places, none of them a storage witness on the sequence
  step: `input_source_commitment` binds the argument list to the record and the record is
  fingerprint-bound to `commit.bin`; `verify_step_record_inputs` holds those arguments to the CFS;
  and the **consuming** step — which has roots — proves the coordinates are in the store and the
  selection resolves. `checks/cfs.rs`'s `verify_sequence_scope_parent` plus `assert_record_in_trace`
  pin "argument `i` of the parent" to the parent's own record. The obligation is discharged where
  consumption happens, which is the only place there is an authenticated root to check against.

  Open nuance, untraced: a recur sequence *iteration*'s `SequenceStart` returns early from
  `verify_step_record_inputs` into `verify_recur_iteration_chunking`, so the generic CFS binding
  check is skipped there in favour of the recur rules. Whether those cover the same ground is worth
  its own look.

- ~~**A recur site's source is recorded as a raw payload, not list metadata**~~ — **fixed
  2026-09-18.** `checks/cfs.rs` → `Recur site start … must commit to list metadata, not a raw
  payload` (`left: Raw, right: List`) at the `call_recur_seq!` site `[3, 0]`.

  `lazy-list-recur` §2 was implemented for the recur **tile** macro and never for the recur
  **sequence** one, though §3 says it binds both. `call_recur!` traced its source through
  `recur_source_trace` (`recur.rs:745`, `:870`); `call_recur_seq!` used `auth_ref_trace`
  (`:1376`) — while carrying, verbatim, the comment describing the `0x0A` metadata selection it
  did not make.

  **Two costs, one line.** `authenticated_source_len` refuses anything but `List`, so no fraud
  proof covering a recur *sequence* site could be produced at all. And `auth_ref_trace` resolves
  the binding, materializing the whole list before any runner runs — §2's *"earliest and largest
  of the three eager paths"*, the very thing that proposal's headline change exists to remove. The
  recur sequence had been paying the full `O(list)` eager resolve the whole time.

  **How it was found.** Two wrong diagnoses first, both from reasoning instead of measuring: that
  the program side never produces `List` (a grep for the literal enum variant missed
  `recur_source_trace`'s indirect path), and that both recur macros already called it (two call
  sites seen, assumed one per macro — both are recur-*tile* variants). The measurement that settled
  it: the site's `"input"` binding was byte-identical to the enclosing sequence's `pieces`
  argument, `selected_len = 441` where metadata is a constant 41.

  **Verified:** `a_recur_sequence_site_commits_to_its_source_list_metadata` (`raster/tests/
  recur_draft.rs`) asserts `payload_kind == List` and `selected_len == 41` through the macro rather
  than a hand-built fixture — and reverting the one-line fix makes it fail with the production
  symptom (`left: Raw, right: List`), so it is not tautological. `prompt-prepare` regenerated its
  commitment honestly (exit 0) and re-audited. Error message corrected too: it named `call_recur!`
  on a path reachable from either macro.

  **Cost:** every recur-sequence site's recorded binding changes, so `input_source_commitment`,
  the step record, the trace leaf and the fingerprint all move. Any existing `commit.bin` for a
  program using `call_recur_seq!` must be regenerated.

- **A window step is not where the CFS expects it** — newly reached, and the current blocker:
  `checks/cfs.rs` → `Step coordinates are not in expected next coordinates`, two steps into a
  window. The injector places its tamper by trace index, so the regenerated commitment moved the
  window to a different part of the trace; whether this is a property of that region or of the new
  window's opening position is the first thing to establish.

- **Why a `SequenceStart` needs no storage evidence at all** (recorded because an earlier draft of
  this file claimed the opposite). A step with no `StorageRoots` never proposes a root: the guest
  returns its own carried frontier unchanged, and every step that *does* declare roots must match
  it (`Execution-step storage root before does not match current storage root`). So a sequence step
  is transparent to the storage chain — it cannot alter the root because it is never asked for one,
  and giving it roots would be *worse*, handing a prover somewhere to propose one.

  Its arguments are authenticated in three places, none of them a storage witness on the sequence
  step: `input_source_commitment` binds the argument list to the record and the record is
  fingerprint-bound to `commit.bin`; `verify_step_record_inputs` holds those arguments to the CFS;
  and the **consuming** step — which has roots — proves the coordinates are in the store and the
  selection resolves. `checks/cfs.rs`'s `verify_sequence_scope_parent` plus `assert_record_in_trace`
  pin "argument `i` of the parent" to the parent's own record. The obligation is discharged where
  consumption happens, which is the only place there is an authenticated root to check against.

  Open nuance, untraced: a recur sequence *iteration*'s `SequenceStart` returns early from
  `verify_step_record_inputs` into `verify_recur_iteration_chunking`, so the generic CFS binding
  check is skipped there in favour of the recur rules. Whether those cover the same ground is worth
  its own look.

- **A recur site's source is recorded as a raw payload, not list metadata** — newly reached, and
  the current blocker:

  ```
  checks/cfs.rs → Recur site start StepRecord { exec_index: 735,
      sequence_id: "merge_prompt_piece", coordinates: CfsCoordinates([3, 0]),
      kind: SequenceStart { .. } } must commit to list metadata, not a raw payload
    left: Raw    right: List
  ```

  `authenticated_source_len` requires the site's `"input"` binding to commit to the `0x0A`
  `(len, elements_root)` record. That strictness is load-bearing, not incidental: it is what makes
  `L` *authenticated* rather than index-trusted, and it is the check that refuses the forged
  `len = 0` sweep `lazy-list-recur` exists to close. A `Raw` payload cannot satisfy it even in
  principle — `decode_list_metadata_len` parses the `0x0A` form and a raw payload is not one.

  **Not yet diagnosed.** An earlier draft here claimed the program side never produces `List`,
  from a grep for the literal enum variant in `raster-macros`/`raster`. That grep was too shallow
  and the claim is **wrong**: `raster/src/input.rs`'s `recur_source_trace` builds the binding from
  `stored_list_metadata`, whose commitment is `List`-kinded, and *both* recur macros call it
  (`raster-macros/src/recur.rs:745` and `:870`, each with a comment citing `lazy-list-recur` §2).
  So §2 is implemented and the site start should already record `List`.

  What is established: the failing record is the recur **site** start (`[3, 0]` is main's item 3 →
  `merge_round` → its child 0, the `call_recur_seq!` site; iterations are `[3, 0, i]`), the
  `"input"` binding is present, and its recorded `payload_kind` is `Raw`. Why a binding built by
  `recur_source_trace` arrives as `Raw` is the open question — measure the recorded binding rather
  than reason about it, which is what the wrong claim above cost.

  A related correction: an even earlier draft guessed this was specific to a produced-list source
  because "the site at `[2, 0]` passed". That evidence does not exist — `[2, 0]` passed only as the
  site-*close* `Exec` step, and `authenticated_source_len` runs only on `SequenceStart`.

  **The tests still hand-build the shape.** `seed_recur_source` inserts
  `metadata.selected.commitment` directly rather than exercising `recur_source_trace`, so whatever
  is happening between that function and the recorded trace is not covered by any recorder test.

  **Directions** (not picked): build the site's `input` binding from a metadata selection in
  `call_recur_seq!`/`call_recur!`, so the recorded commitment describes what the site actually
  consumes; or patch the binding in the recorder when it emits the site `Start`, where
  `recur_source_len` already holds the metadata and where `input_source_commitment` is computed
  over the same (patched) input. The first is what `lazy-list-recur` §1–§2 describes; the second is
  smaller but makes the recorded binding differ from what the program published.

## 3. What this is not

- **Not a soundness hole.** It fails closed: nothing here lets a receipt exist against an honest
  prover. It is a completeness defect, and in an optimistic protocol a completeness defect on the
  dispute path is still a lost dispute.
- **Not window geometry.** `fraud-window-trace-ends` is closed; its head and tail cases were about
  the window's *length*. The divergence here is mid-trace, the window is a full 32 items, and the
  failure is after the window is built.
- **Not the `witness_record_inputs` panic.** That one is fixed (`0a71d10`) and sits earlier, inside
  `verify()`. This is the next site in the same path — see §5's first direction.
- **Not the `revealed_items` asymmetry.** Byte 5000 flipping clean is
  `fraud-window-trace-ends` §4 restated: the audit reads `revealed_items` only by `.len()`, and its
  contents are bound on the transition-guest path instead. Still true after the `v2` header work,
  which added `window_size` but never made the guest open `revealed_items_commitment`. A separate
  observation, recorded here only because the same sweep surfaced it.
- **Not `--no-auth`.** That mode emits no trace, so there is nothing to prove or refute.

## 4. What it costs

For `prompt-prepare` — a real program, not scaffolding — the fraud-proof path currently reaches a
verdict and then aborts. What an operator sees is a stack trace at exit 101, not a receipt.

`chain-io-commitment` §3's dispute protocol is not built yet, so nothing settles on this today.
When it is, this is the difference between condemning a cheat and timing out.

## 5. Directions

Shapes only; none is picked. The first two are diagnostics, and both should run before any fix is
designed — the third and fourth differ by which one is true.

- **Bisect against `0a71d10`.** `witness_record_inputs` runs inside `verify()`, so it fires before
  `prove()`. Check out `63980a9`, regenerate a v1 commitment, flip a mid-fingerprint byte: if it
  dies earlier with `"Failed to resolve active sequence invocation"`, this is the next link in one
  chain and the chain is longer than `fraud-window-trace-ends` §5 assumed. Cheap, and it decides
  whether to expect more sites behind this one.
- ~~**Name the failing step.**~~ **Done 2026-09-16** — see §2. Neither hypothesis held; the answer
  was a single binding whose recorded source coordinate names a step that never executed and has
  the wrong arity for its position. The diagnostic patch (report every binding rather than
  panicking on the first; check the source against the trace) is not committed — re-apply it in
  `build_storage_selection_witnesses` (`run.rs:737`) if needed.
- **Make the runtime address storage the way the recorder does** (§2b). In
  `reserve_execution_coordinates`, the recur branch must apply only where it is correct — a recur
  *tile*, whose iteration genuinely is one tile execution and which pushes no `SequenceFrame`.
  Inside a recur *sequence* iteration the right frame is already on the stack; prefer it. Two
  shapes: give `RecurFrame` a site kind, or give it an `iteration_open` flag set by
  `enter_recur_sequence_iteration`/`exit_recur_sequence_iteration` — the latter names the exact
  condition. Either way `next_iteration_index` must stop being advanced by body tiles, since
  `enter_recur_sequence_iteration` reads that same counter and its iteration numbering drifts too.
  **Costs:** every storage coordinate inside a recur-sequence body moves, so recorded traces and
  every commitment over them change.
- **Or reconcile the two maps rather than the two schemes.** `TraceRecorder.storage` and
  `THREAD_STORAGE` are separate `StorageManager`s populated by different paths. Whether they are
  *meant* to agree key-for-key is a design question this issue does not settle; if they are, the
  divergence is a bug in whichever path is wrong, and if they are not, then resolving a recorded
  `StorageRef` against the recorder's map is the mistake and the fraud path needs the runtime's.
- **Decide what a source coordinate with no stored object means.** If the trace can legitimately
  record one, evidence construction must tolerate it — and `selected_len == 0` (`:751`) is already
  a precedent for skipping a binding, so the question is whether this is a second legitimate skip
  or a recorder defect that should never have emitted the binding. Costs: skipping wrongly hands
  the guest a window missing a witness it needs, which moves the failure into the zkVM.
- **Return an error rather than panicking.** `run.rs:767` is `panic!` inside `unwrap_or_else`, and
  `prove()` already sits in a `Result`-returning path. Strictly an improvement in reporting and
  strictly not a fix: the proof still cannot be built. Worth separating from whatever closes the
  underlying gap, so that "no longer crashes" is not mistaken for "now provable".
- **Reconstruct the witness from the trace instead of live storage.** The recorded
  `StorageData` carries the coordinates, commitment, selector and selection commitment; only the
  payload is missing. If that is derivable from the trace the auditing process already holds, the
  live map stops being the authority. If it is *not* derivable, the honest consequence is worth
  stating plainly: a challenger would need data only the accused holds, which is a
  data-availability dependency — see `raster-prover::availability`, which names that assumption.

## 6. Reproducing

```bash
cd raster-inference/prompt-prepare
cargo raster run --input input.json --input-manifest input_manifest.json \
  --commit /tmp/c.bin --fraud-proof-window-size 32

# The supported path: the built-in injector, which corrupts one executed step.
cargo raster run --input input.json --input-manifest input_manifest.json \
  --commit /tmp/fraud_demo.bin --fraud-proof-window-size 32   # see run.rs:528
cargo raster run --input input.json --input-manifest input_manifest.json \
  --audit /tmp/fraud_demo.bin
```

Or by hand, on the packed fingerprint rather than the header:

```bash
python3 -c "
import pathlib
b = bytearray(pathlib.Path('/tmp/c.bin').read_bytes()); b[100] ^= 0x01
pathlib.Path('/tmp/mid.bin').write_bytes(bytes(b))"
cargo raster run --input input.json --input-manifest input_manifest.json --audit /tmp/mid.bin
```

Two adjacent outcomes worth not confusing with detection, both reachable from the same sweep:
flipping byte 0 changes `bits_packer` and is refused by `validate()`; flipping with `^ 0xFF`
rather than `^ 0x01` clears postcard's varint continuation bit and is refused by the decoder.
Both exit non-zero without a fingerprint ever being compared.

`prompt-prepare/fraud_demo.bin` is left untracked in the tree from the reproduction;
`prompt-prepare/commit.bin` is the honest regenerated one and was not modified.
