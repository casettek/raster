# Proposal: `live-recorder-shared-objects` — a live recorder reads the program's objects instead of copying them

Status: **Proposed 2026-11-04. Step 0 measured the same day** — §Measurement. The duplicate costs
19–30% of a live run's heap. In audit the largest item is something else: `SourceTracker` keeps a
`StepRecord` for every producing step of the run (§Finding: `SourceTracker` retains every
producer).

Related:
- [`storage-role-split`](./storage-role-split.md) — split one `StorageManager` into the program's
  `ObjectStore` and the recorder's `AuthenticatedObjectStore`, when the two lived in different
  processes. This proposal keeps that split of *roles* and removes the duplication of *bytes* that
  it implied once both stores ended up in the same process.
- [`storage-write-cost`](./storage-write-cost.md) — is about the time a write takes. This proposal
  is about the memory each write keeps.

## Problem

On main, a `cargo raster run --commit` is two processes. The program writes `trace.bin`. The CLI
then rebuilds every object from the trace events in its own `AuthenticatedObjectStore`. Two copies
of every object were unavoidable, because the copies were in different processes.

On `codex/windows-native`, live `--commit` and `--audit` install the recorder **inside the program
process**:
- the `CommitmentPublisher` uses `TraceRecorder::for_commitment_with_storage(…, Memory)`
  (`tracing/publishers/commitment.rs:32-34`);
- the `AuditPublisher` uses `TraceRecorder::for_live_audit` (`tracing/publishers/audit.rs:62`).

Both still rebuild their own replica of every object, as the recorder did in the CLI. The code
says so: `AuthenticatedObjectStore::append_output` builds a derived object "over the base this
store already holds — the recorder's own replica, never the child's" (`storage.rs`, above
`append_output`). The only thing a live recorder takes from the program's store is the
entry-argument resolver (`commitment.rs:37`, `audit.rs:64`).

### The copies, per object, in a live run

A tile output takes this path:
1. The tile wrapper encodes the return value twice, as postcard bytes and as a raster payload
   (`raster-macros/src/lib.rs:2741`).
2. It stashes a copy of both for the program's store (`raster/src/lib.rs:542`).
3. It puts both into the `TileExec` event.
4. The recorder appends the event's output to its own store: `append_serialized_bytes(&output.data, …, output.raster.clone())`
   (`recorder.rs:1028`, `:1114`).
5. The sequence binding then stores the value in the program's store, reusing the stashed raster
   payload (`store_execution_output_value`, `storage.rs:1550`).

A recur site's close goes the other way: the program stores the object first, then
`stored_object_output` clones its payload into the `RecurTileEnd` event
(`storage.rs:1871`, called from `raster-macros/src/recur.rs:772`). The recorder appends it again
(`recorder.rs:1245`, `:1344`).

What stays resident for each object:

| | program `ObjectStore` (`THREAD_STORAGE`, `storage.rs:1156`) | recorder `AuthenticatedObjectStore` |
| --- | --- | --- |
| postcard bytes | yes (`owned_backing`, `storage.rs:248`) | yes |
| raster payload (bytes + index bytes) | yes | yes |
| parsed index (after the first read) | yes | yes |
| log position + commitment | — | yes (frontier, coordinate index) |

So a live run holds **two full copies** of every object, each in **two encodings**. The event and
the stash are transient copies on top of that.

### What the recorder actually needs

The recorder's store has three kinds of use:
- **Writes**, which need only the object's *commitment*: `append_serialized_bytes`,
  `append_output` and `load_authorized_sources` (`recorder.rs:1478`). These feed the storage log
  frontier, the coordinate index and the per-step roots.
- **Roots**, via `snapshot()`, which come from the frontier and the index only.
- **Reads**, which need the object's *bytes*:
  - `selection_witness` (`recorder.rs:515`), used for storage-input witnesses and by
    `proof_input::prepare_step_io` in live audit;
  - `list_metadata_selection` at a recur site's open (`recorder.rs:1543`);
  - `derivation_base` inside `append_output` for a derived output.

Every read is of an object an **earlier** step wrote, and the program has stored it, because the
program read it to run the current step. The current step's own output is never read back by the
recorder during the same `record`.

## Measurement

Measured 2026-11-04 on `codex/windows-native` @ `af2a47b`, in a throwaway worktree pair under
`~/.cache/raster-mem-bench/`. A counting `#[global_allocator]` in `raster-runtime` tracked live and
peak heap. At the end of the run, each component was dropped in turn and the heap shrank by
exactly what it held. Parsed indexes were measured by re-parsing exactly the indexes that were
cached. The probe patch is kept as `~/.cache/raster-mem-bench/mem-probe.patch` plus `mem_count.rs`.
Nothing in it is committed.

Workloads:
- **hello-tiles, 20,000 address lines** (`gen_input <dir> 20000`): 60,050 stored objects, 280 B
  of payload each. A long recur run with small objects.
- **Iris `prefill_range_l0`** on the tiny dev model (`bench-iris/pr-inputs`): 1,149 objects, about
  2,000 steps. The full Gemma weights are not on this machine.

Window 128 throughout. Audit ran against the honest commitment, so it executes the whole program
(`no-fault`).

Heap at the end of the run (bytes held when each component is dropped):

| | hello-tiles commit | hello-tiles audit | Iris commit | Iris audit |
| --- | --- | --- | --- | --- |
| **heap peak** | **171.4 MB** | **371.7 MB** | **6.9 MB** | **19.9 MB** |
| program `ObjectStore` | 93.3 MB | 93.3 MB | 3.4 MB | 3.4 MB |
| — of which parsed indexes | 26.5 MB | 26.5 MB | 0.6 MB | 0.6 MB |
| **recorder's object replica** | **46.1 MB (27%)** | **70.7 MB (19%)** | **2.1 MB (30%)** | **2.7 MB (13%)** |
| — of which parsed indexes | 0 | 23.7 MB | 0.04 MB | 0.6 MB |
| recorder coordinate index | 28.5 MB | 28.5 MB | 0.5 MB | 0.5 MB |
| audit `SourceTracker` | — | **140.7 MB (38%)** | — | 1.5 MB |
| audit `ProofStorageState` (storage-log tree + a second coordinate index) | — | 32.7 MB | — | 0.6 MB |
| audit sliding window (128 steps) | — | 2.0 MB | — | **9.0 MB (45%)** |
| unattributed | 0.03 MB | 0.3 MB | 0.05 MB | 0.4 MB |

Peak RSS from `/proc/self/status` `VmHWM`:

| | unauthenticated | live commit | live audit |
| --- | --- | --- | --- |
| hello-tiles 20k | 29.9 MB | 200.5 MB | 422.6 MB |
| Iris `prefill_range_l0` | 13.4 MB | 17.4 MB | 32.4 MB |

What this shows:
- **Payload bytes are a small part of an object's cost.** hello-tiles' objects hold 16.8 MB of
  payload (postcard 0.9, raster bytes 4.8, index bytes 10.2, derived tail 0.9). The program's
  store holds 93.3 MB for them and the recorder's replica 46.1 MB. The rest is per-object structure
  (map nodes, coordinate vectors, `StorageRef`s, `Arc`s), parsed indexes and, on the program side,
  probably spare `Vec` capacity in the encodings (not verified).
- **The replica's cost grows when the recorder reads.** Audit builds selection witnesses, which
  parse each read object's index into the replica's own `OnceLock`: +23.7 MB on hello-tiles. A
  shared store parses each index once.
- **Removing the replica** saves 27% (commit) and 19% (audit) of heap on hello-tiles, and 30% and
  13% on Iris. That is roughly the whole recorder replica plus its parsed indexes.
- **The postcard copy (plan step 2) is small:** 0.9 MB on hello-tiles and 0.7 MB on Iris.

### Finding: `SourceTracker` retains every producer

`SourceTracker` (`raster-prover/src/trace.rs`) is the audit publisher's record of where each
step's inputs came from. Its doc comment says "source records indexed by active frame", but
`append` inserts a full `StepRecord` clone into `producers` for every `Exec`, `SequenceEnd` and
`RecurEnd`, keyed by `(frame_start, coordinates)`, and nothing removes entries when a frame closes.
A recur sequence re-opens its frame every iteration, so the map grows with every iteration of the
run. On hello-tiles that's 140.7 MB, the largest item in a live audit.

A `PriorItemOutput` binding only ever names a producer in the step's *current* frame instance, so
entries for a closed frame instance can be dropped when it closes. That needs its own small change
and regression test, against the `live_commitment_cli` comparison with recorded-trace evidence, and
is independent of this proposal. It should go first: it's the largest single saving measured.

### Other duplicates found
- **Two coordinate indexes in live audit.** The recorder's `AuthenticatedObjectStore` and the
  audit publisher's `ProofStorageState` each maintain an `IncrementalCoordinateIndex` over the same
  writes: 28.5 MB each on hello-tiles. The proof state could read the recorder's.
- **The program's parsed indexes are cached forever.** 26.5 MB on hello-tiles. That's on main too
  (`RasterObject::index`), and a separate decision.

## Proposal

In the live posture only, the recorder's `AuthenticatedObjectStore` keeps its frontier, coordinate
index and cached root as today, but **holds no object bytes**. Its reads go to the program's
`ObjectStore`.

### Mechanism
1. **An explicit object source.** `AuthenticatedObjectStore.objects` becomes one of:
   - `Owned(ObjectStore)` — today's behaviour, unchanged. Used by every offline recorder (CLI
     replay of `trace.bin`, Disk) and every test that replays events outside a program.
   - `Program` — reads through `THREAD_STORAGE`. Selected only by the two publishers through a new
     constructor, for example `TraceRecorder::for_live_audit_shared` and
     `for_commitment_shared`.

   It must **not** follow the Memory policy. The `live_commitment_cli` oracle builds a Memory
   recorder outside any program (`raster-cli/tests/live_commitment_cli.rs:339`) and has no program
   store to read from.
2. **Writes.** The recorder computes the commitment exactly as today (`internal_object_commitment`,
   or the derived object's root), and updates the frontier and index. It drops the bytes.
   - For a derived output, it still builds the `DerivedObject` from the delta over the base, now
     read from the program's store, to validate the delta and take its root. Then it discards it.
     The pieces are `Arc`s into the program's buffers, so this copies only the delta's tail.
3. **Reads.** Before delegating, the recorder checks that the reference is one it logged: the
   coordinate index has an entry at `reference.coordinates` and its `object_commitment` equals
   `reference.commitment` (`IncrementalCoordinateIndex::get`). Then it calls the same `ObjectStore`
   method on `THREAD_STORAGE`, and that store's `verify_reference` re-checks the object's integrity.
4. **Event payloads.** The recorder moves `fn_call_record.output` out of the event instead of
   cloning `output.raster` (`recorder.rs:1028`, `:1114`). With step 2 of the plan, it doesn't keep
   it at all.

### Why this is sound
- **Same process, same thread.** The publisher is called synchronously from the tile wrapper and
  the recur macros. Native execution is single-threaded; the only spawned thread is the profile
  writer (`profiling.rs:844`).
- **The program's store is write-once and never shrinks.** `ObjectStore::put` asserts on a second
  write to a coordinate, and nothing removes from `memory`/`referenced`.
- **The check is the same as today.** Today a recorder read succeeds only for an object in the
  recorder's replica, which contains exactly the logged objects. The index-entry check in mechanism
  3 keeps that guarantee. The program's own integrity check then fails loudly on any byte mismatch,
  which today would surface the same way: as a commitment mismatch.
- **Nothing committed changes.** Commitments, roots, step records, witnesses and the trace format
  are computed from the same values in the same order. `commit.bin`, audit handoffs and receipts
  must be byte-identical. That's the acceptance test.

### What does not change
- **Offline paths** (plain `run`, `trace commit`, `trace audit`, `--fraud-step`, chain stage
  re-runs): the recorder runs in the CLI with no program store, so it keeps its own replica (Disk).
- **`--save-trace`**: the trace-file tee still receives full events.
- **Guests, `raster-core` and image IDs**: untouched. All changes are in `raster-runtime`.

## Plan
0. **Measure.** Done 2026-11-04, §Measurement. The replica is 13–30% of a live run's heap, so the
   proposal stands. The measurement also ranked what to do first:
   - `SourceTracker` eviction (§Finding), outside this proposal: up to 38% of audit heap.
   - Shared reads (step 1): 19–30%.
   - One coordinate index instead of two in audit (§Other duplicates): 8% of audit heap on
     hello-tiles.
   - The postcard copy (step 2): under 1%.
1. **Shared reads** (mechanism 1–4). Acceptance:
   - `commit.bin` and the audit handoff are byte-identical to the current branch on hello-tiles,
     the chain example and the step-0 program;
   - the `live_commitment_cli` comparison of live evidence against recorded-trace evidence still
     passes;
   - re-running the §Measurement probe, the recorder's object line drops to near zero (46.1 MB →
     ~0 on hello-tiles commit) with nothing else rising.
2. **No postcard copy for raster objects** (measured small, under 1%; do it only for tidiness). `owned_backing` keeps empty postcard bytes when a raster
   payload is present, as `store_sealed_at_site` already does (`storage.rs:1802`). Every read of a
   raster object goes through its payload (`resolve_whole`, `raster_view`). The integrity check
   uses the root (`internal_object_commitment`, `storage.rs:240`), and the archive's
   `PersistedBacking` doesn't need the bytes. This applies in every posture, including no-auth and
   offline replay.
3. **Optional: fewer transient copies.** The stash clone and the second postcard encoding in
   `store_execution_output_value` exist only to hand the wrapper's encoding to the store. Measure
   them in step 0 before touching them.

## Risks and open questions
- **Re-entrancy.** A `Program` read borrows `THREAD_STORAGE`. Today no event is published while
  that cell is borrowed: `stored_object_output` returns before the macros publish, and no
  `publish_trace_event` call sits inside a `THREAD_STORAGE.with`. A debug assertion, or
  `try_borrow` with a clear error, should guard against a future change that breaks this.
- **Selective profiling.** `CommitmentPublisher` in capture mode uses `for_live_audit`.
  `selective_profile::on_step` reads payloads through `step_witness_at` (the witness store), not
  the object store, so it is unaffected. The capture fixture should still be part of step 1's
  acceptance run.
- **Failure semantics.** A program that panics mid-run drops nothing the recorder still needs: the
  commitment is never published and the audit handoff is rejected (`validate_handoff`), as today.
- **Disk mode.** `Program` mode never uses the archive. The `Archived` backing and loaded-object
  cache serve only `Owned` recorders.
