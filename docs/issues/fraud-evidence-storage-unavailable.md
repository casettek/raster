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
- **The transition guest cannot decode the `ProgramDefinition` frame** — newly reached once the
  lock was refreshed, and the current blocker:
  `guests/transition/src/fraud_proof.rs:64` → `failed to decode ProgramDefinition: Found a bool
  that wasn't 0 or 1`, surfacing host-side at `raster-prover/src/transition.rs:289`. A postcard
  misalignment of that shape means the host and the guest disagree about the struct's layout,
  which normally means the embedded guest ELF was built against a different `raster-core`. Worth
  ruling out a stale risc0 build cache first, since `RISC0_SKIP_BUILD=1` is used freely during
  development and this workspace has moved `raster-core` repeatedly.
- **Replay journal decode failures** — `Failed to decode replay journal: Hit the end of buffer`
  for every tile, non-fatal (`run.rs:1002` prints and continues). Consistent with prebuilt tile
  guests predating a `raster-core` change; a clean guest rebuild would confirm.

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
