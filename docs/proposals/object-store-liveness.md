# Proposal: `object-store-liveness` — release a stored object once nothing can read it

Status: **Proposed 2026-11-12 — step 0 measured.** The measurement probe lives only in the bench
copy (§Reproducing); nothing in this tree changed.

Related:
- [`live-recorder-shared-objects`](./live-recorder-shared-objects.md) — removed the recorder's
  second copy of every object, so a live run holds exactly one: the program's `ObjectStore`. This
  proposal is about that one copy, which is never released.
- [`storage-ledger-checkpoints`](./storage-ledger-checkpoints.md), `5f46131`, `541cd24` — bounded
  the audit's *bookkeeping* to the active frame and the last W steps. This proposal applies the same
  rule to the *objects*.
- [`instrumentation-pipeline`](./instrumentation-pipeline.md) — the recorder runs up to 64 events
  behind the program (`worker.rs:30`); release must wait for it (§Finding 5).
- [`clone-free-bindings`](./clone-free-bindings.md) — `select!` and `call!` take bindings by
  value. That is why dropping a handle tracks the last read so closely (§Finding 3).
- [`incremental-draft-materialization`](./incremental-draft-materialization.md) — derived objects
  read their base at the site's close, which is the one reader no binding covers (§Finding 4).

## Problem

Every tile output, recur site object and entry-argument binding a live run writes stays in memory
until the process exits. `ObjectStore::put` (`raster-runtime/src/storage.rs:404`) inserts and
nothing removes; the type documents it: *"objects are written once and never removed"*
(`storage.rs:103`).

So the store grows with the length of the run, even though most objects are dead long before the
end. A sequence variable holds an `AuthRef::Storage` — a coordinate and a commitment — and once the
last binding naming a coordinate is consumed, the program has no way to name that object again.
In hello-tiles, 40,000 of the 60,050 objects are recur-sequence iteration outputs that are read
once, by the next step, and never again.

Everything else a live commit or audit keeps has already been bounded:

| state | bound | where |
| --- | --- | --- |
| step witnesses | released per step / at frame close | `release_live_step`, `recorder.rs:547` |
| `SourceTracker` producers | dropped when their frame closes | `541cd24` |
| proof evidence | only for the divergent window | `5f46131` |
| coordinate-index undo | last W writes | `storage-ledger-checkpoints` |
| commitment | fingerprint bits + W records + W roots | `commitment_file.rs:28` |
| **object bytes** | **none — whole run** | `storage.rs:404` |

## Measurement

**Method.** A probe in a `git archive` copy of `b61c764` (raster) and the live iris:
- **Heap.** A counting global allocator. The probe's own bookkeeping is excluded with a
  thread-local flag, so heap samples are the program's heap alone.
- **Clock.** One tick per trace event the program publishes; the heap is sampled at every tick.
- **Writes and reads.** The probe records each object's write tick, and each read through
  `verify_reference` (`storage.rs:599`, the gate every `resolve`, `select`, derivation and output
  publish goes through) with the reading thread.
- **Scopes.** Each sequence, recur-site and recur-iteration frame instance is recorded with its
  open and close ticks.
- **Handles.** The number of live `DeferredAuthStorage` handles (`AuthRef::Storage`, including
  `BoundIndex` citations) per coordinate, counted on creation, `Clone` and `Drop`.
- **Object sizes.** After `ProgramEnd`, every object is dropped newest-first and the heap delta is
  recorded, which gives the exact retained size of each object.

The probe adds no RSS when disabled and changes no output. `commit.bin` and the audit handoff are
byte-identical to the unprobed build on every workload below.

**Policies.** Each policy gives a release tick per object; the projected heap at tick t is the
measured heap minus the objects released before t.

| policy | an object is released |
| --- | --- |
| today | never |
| after last read (oracle) | right after its last read by any thread — not implementable, a lower bound |
| last handle drop | when its last `AuthRef` handle drops — **what this proposal implements** |
| end of binding scope | when the innermost frame enclosing the writer and every reader closes — lexical scope |
| `+ W` | any of the above, held W = 128 events longer — what live audit needs (§Finding 6) |

### Results

Live commit, window 128. Baseline RSS is the unprobed binary's peak (`getrusage`), the median of 3
runs for 20k and Gemma and one run for 40k and 80k; heap figures are from one probed run:

| workload | objects | store at end | heap peak today | last handle drop | `+ W` | after last read | end of binding scope | baseline RSS |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| hello-tiles 20k | 60,050 | 75.9 MiB | 118.5 MiB | **47.4 MiB** | 47.4 MiB | 42.7 MiB | 90.0 MiB | 147 MB |
| hello-tiles 40k | 120,050 | 151.8 MiB | 238.3 MiB | — | — | 86.5 MiB | 181.2 MiB | 286 MB |
| hello-tiles 80k | 240,050 | 303.8 MiB | 478.1 MiB | **193.2 MiB** | 193.2 MiB | 174.3 MiB | 363.9 MiB | 558 MB |
| Gemma `prefill_range_l0` | 585 | 11.6 MiB | 13.1 MiB at an event, 110.1 MiB between events | 2.7 MiB | 2.8 MiB | 2.7 MiB | 3.1 MiB | 257 MB |

(40k ran on the first probe, before handle counting.)

Live audit (honest) shows the same differences. On hello-tiles 20k the heap peak is 136.3 → 65.2
MiB with last handle drop + W, and on Gemma it is 47.4 → 37.1 MiB. Baseline RSS is 163 MB
(hello-tiles 20k), 315 MB (40k), 611 MB (80k) and 286 MB (Gemma).

### Findings

1. **The store is the largest thing a live hello-tiles run holds, and it grows linearly.** It is
   64% of the heap peak at every size (75.9 of 118.5, 151.8 of 238.3, 303.8 of 478.1 MiB), and the
   peak is at the end of the run, because the store only ever grows. Releasing on last handle drop
   cuts the heap peak by 60%: 118.5 → 47.4 MiB at 20k, 478.1 → 193.2 MiB at 80k.

2. **Lexical scope is the wrong rule.** It saves only 24%. `main` encloses the readers of 19 large
   objects — the recur outputs at `[9]`, `[10]` and `[13]`, 25 MiB at 20k — so the scope rule holds
   them to the end. `main` reads each of them once, one event after it is written. The scope that
   matters is the binding's, not the frame's.

3. **Handle drop is within 10% of the oracle,** because bindings are moved: `select!` takes its
   base by value (`input.rs:1031`), and a tile's generated parameters are `impl IntoAuthRef<T>`
   taken by value (`raster-macros/src/lib.rs:3438`). The gap is objects
   whose binding is cloned and kept for a later `select!` (hello-tiles `draft_greeting.clone()`,
   `address_lines.clone()`). No object was still held at the end except `main`'s output. The most
   handles one object had at once was 5 on hello-tiles and 31 on Gemma.

4. **The program never reads an object after its last handle drops; the recorder does, in two
   places.** Releasing at zero would make these reads fail. Both are on the recorder thread, which
   holds a plain `StorageRef`, not a binding:
   - **a derivation base, at the deriving site's close.** The recorder rebuilds the derived object
     from its base (`derived_backing` → `derivation_base`, `storage.rs:485`, `:472`). On
     hello-tiles, base `[12]` is read at derived `[13]`'s close, 80,001 events after its last
     handle dropped. Gemma has a chain of bases (`[12]`, `[13]`, `[14]`, up to 431 KB) read 1 to
     8,533 events late. The program itself took its view of the base when the site opened, so its
     handle is long gone.
   - **`main`'s output, at `ProgramEnd`.** Hello-tiles `[23, 7, 4, 1]` is read by the recorder one
     event after its handle drops.

   No handle was revived after reaching zero. `storage!` from a raw `StorageRef` appears only in
   tests (`raster/tests/draft_selection.rs`, `unauthenticated_execution.rs`,
   `iris/tests/tile_chunk_support.rs`), never in a program.

5. **The two late reads need different fixes.** The output read lags by the worker queue (at most
   64 events, `worker.rs:30`), so waiting for the recorder's progress covers it (§Design 2). The
   base read lags by the length of the deriving site, which is unbounded, so the base needs its own
   pin until the recorder has recorded the site's close.

6. **Live audit must keep the last W steps' inputs.** At a divergence the audit builds evidence for
   the window it holds (`audit.rs:192-200`), which reads the objects the window's steps read. An
   honest run never diverges, so the measurement does not exercise this; `+ W` accounts for it, and
   on these workloads costs nothing measurable (47.4 vs 47.4 MiB), because the peak is at the end of
   the run and not inside a loop.

7. **What stays linear.** With every object released, the hello-tiles heap still grows by about
   560 B per write: 174.3 − 45.9 MiB over 240,050 writes at 80k. That is mostly the recorder's
   coordinate index, measured at 475 B per write on 2026-11-04 (28.5 MB for 60,050 writes,
   [`live-recorder-shared-objects`](./live-recorder-shared-objects.md)), plus the 32 B log leaf.
   The commitment needs that index, so this proposal does not touch it. Also outside this proposal:
   the one object that legitimately grows with the input — the 80k-element derived list, 45.9 MiB
   and live until `main` reads it.

8. **Gemma has nothing to release.** Its weight pages are `Referenced` entry arguments (no bytes
   in the store), and its owned objects total 11.6 MiB. The 110 MiB heap peak is transient — it
   falls between events, during page reads — and RSS (257 MB) is not the heap. This proposal is for
   long programs with many intermediate values, not for model stages.

## Design

### 1. A pin per coordinate

The store keeps, beside each in-memory object, a count of live pins. A pin is an RAII handle
(`ObjectPin`) naming one coordinate: `Clone` increments, `Drop` decrements. Each of these owns one:

| holder | released when | covers |
| --- | --- | --- |
| `DeferredAuthStorage` (`input.rs:1071`) — every `AuthRef::Storage`, every `select!` result | the binding drops | ordinary reads |
| each `BoundIndex` citation in `index_bindings` | the citing binding drops | the cited source's selection witness |
| a deriving site's base | the recorder has recorded the site's close | Findings 4 and 5 |
| a recur site's source list | the site closes (it is an `AuthRef` today; listed so a refactor keeps it) | `RecurProgressFrame.source` |

A pin has no counterpart in the trace: it is host-side state, like an `Arc` count. A guest never
sees it.

### 2. Release is deferred until the recorder has caught up

When a count reaches zero, the store stamps the coordinate with the current event index and puts
it on a release queue. The recorder publishes the index of the last event it has fully processed
(one atomic). An entry is freed once

```text
stamp + grace ≤ recorder_processed
grace = W for a live audit, 0 for a live commit
```

So the recorder's lag is handled by its real progress, not by a guess at the queue size. That
covers `main`'s output, whose handle drops one event before the recorder reads it at `ProgramEnd`
(Finding 4). And the audit's window evidence (Finding 6) finds every object its window's steps
read.

### 3. A released object leaves a tombstone

Releasing removes the bytes and keeps `coordinates → (commitment, released_at)`:
- the duplicate-write guard in `put` (`storage.rs:419`) checks map membership and has to keep
  working;
- a read of a released coordinate fails with *"object at {coordinates} released at event {n}"*
  instead of "missing storage object". A pin that should have existed fails loudly, never as a
  wrong value.

A tombstone costs about 100 B (coordinates and a 32 B commitment) against the 685 B median object
on hello-tiles. If that matters later, the recorder's coordinate index already holds the same
fact, so tombstones could be dropped then.

### 4. Where it applies

Only in the live publishers, with `WorkingStorage::Memory` and a program-backed ledger
(`commitment.rs:30-36`, `audit.rs:72`). Unchanged:
- **unauthenticated runs** store nothing;
- **offline replay** (`TraceRecorder::new`, `WorkingStorage::Disk`) has no bindings to count.
  Releasing there would need last-read ticks from a pass over the trace;
- **`shared_view`** keeps its semantics — views share the map, so they see a release; the
  comment at `storage.rs:103` changes.

### Why no commitment changes

Releasing removes only bytes. Every root and commitment comes from the recorder's ledger: the
append log and the coordinate index, which store `(coordinates, object_commitment)` and never the
bytes (`storage.rs:942`). A released object's bytes are never read again: that is what the pins
and the grace guarantee, and what a tombstone read error would expose. So `commit.bin` and the
audit handoff must stay byte-identical; that is the acceptance test.

## Steps

0. **Measure** — done, above.
1. **Count without releasing.** Add `ObjectPin` and the counts, plus a check that fails a run on
   any read of a coordinate whose count is zero and whose grace has passed. Run every suite with the
   check on: `raster-runtime`, `raster` integration tests, `live_commitment_cli` (commit and 10
   fault audits, worker on/off/queue 1), the ignored suites (`stage_challenge_cli`,
   `replay_profile_cli`, `fault_estimate_cli`, `chain_repeat_cli`), the chain example and the Iris
   stages. This finds every reader §1 misses before anything is freed. Expected: the derivation
   base of Finding 4, with the output read already covered by §2.
2. **Pin the runtime holders** that step 1 finds.
3. **Release**, with the deferred queue and tombstones. Acceptance:
   - `commit.bin` and handoffs are byte-identical, including the late-fault audits;
   - peak RSS is measured on hello-tiles 20k and 80k, and Gemma is unchanged;
   - the step-1 check stays on in debug builds.
4. **Measure the cost of pins.** Expected to be negligible (at most 31 handles per object
   observed; one map-entry atomic per clone and drop), but measure it against the 2.4 s hello-tiles
   commit.

## Costs and risks

- **An unpinned reader is a failed run.** A release cannot change a commitment, but it can make an
  honest run fail. Step 1 exists for this; the tombstone makes the failure say why.
- **RSS is projected, not measured.** The projections above are heap. Peak RSS should fall by
  roughly the heap saved, because freed objects are reused by later allocations before the heap
  reaches its old peak; allocator fragmentation is the unknown. Step 3 measures it.
- **The win is program-shaped.** Programs that keep large values bound in `main` (or clone a
  binding and hold it) keep them; Gemma gains nothing. The hello-tiles figures are the case this is
  for: long recurs over many small intermediates.
- **A late read by a user is now an error.** A program that saves a `StorageRef` (`.reference()`)
  and rebuilds a binding with `storage!` after the original binding dropped worked before and
  fails after. No program in `examples/` or Iris does this; tests do, but within the binding's
  lifetime. Documenting the rule in the tile-authoring guide is part of step 3.

## Reproducing

Bench in `/data/raster-bench/objlive` (raster `b61c764` and iris HEAD plus its live diff, via
`git archive`; `prefill-range/Raster.lock` deleted so the lock check is skipped):

- `probe/apply.py raster` and `probe/apply_token.py raster` apply the probe (`mem_count.rs`,
  `objlive.rs`, hooks in `storage.rs`, `tracing.rs` and `raster/src/input.rs`);
- `build.sh TAG` builds hello-tiles and `prefill-range` into `bin/TAG` (`base`, `probe2`);
- `run.sh TAG ROUNDS [probe]` runs live commit and honest audit on hello-tiles 20k and Gemma;
  larger inputs come from `gen_input` (`--features gen-input`) in `in/ht{40000,80000}`;
- `RASTER_OBJLIVE=<file>` enables the probe; `probe/analyze.py <file> 128` prints the tables
  above. Results are in `out/`.
