# Proposal: `instrumentation-pipeline` — run the recorder beside the program, not inside it

Status: **Stage 1 implemented 2026-11-06 — measured** (§Stage 1 results). The publisher runs on
its own thread by default: live commit −28%, live audit −30% on hello-tiles 20k, with identical
commitments and handoffs. Stage 0 measured the same day (§Stage 0 results).

Related:
- [`live-recorder-shared-objects`](./live-recorder-shared-objects.md): the recorder already reads
  object bytes from the program's store instead of copying them (`b201b0a`). This proposal takes
  that one step further: the program owns the store, and the recorder reads it from elsewhere.
- [`storage-ledger-checkpoints`](./storage-ledger-checkpoints.md): the storage ledger and its
  rewind. Unchanged in behaviour, only moved.
- [`storage-role-split`](./storage-role-split.md): split the store by role, when program and
  recorder were separate processes (main's design). This proposal returns to two processes, but
  streaming and without copying.

## Problem

In live `--commit` and `--audit` (branch `codex/windows-native`), the user program and all
instrumentation run in **one process, on one thread**. `publish_trace_event` calls the installed
publisher synchronously. While the recorder, the storage ledger, the commitment builder or the
audit (verifier, trace tree, `SourceTracker`, window) handle an event, no tile runs.

Measured on hello-tiles 20k (210,091 steps, 60,050 objects), running the child process directly:

| | wall | of which outside the user program |
| --- | --- | --- |
| program alone, unauthenticated (2026-11-05) | 1.7 s | — |
| live commit (2026-11-06) | ~4.7 s | ~3 s |
| live audit, honest commitment (2026-11-06) | ~6.2 s | ~4.5 s |

After the recent cuts, the instrumentation still costs more than the program on this workload:
about 2.7× in audit, 1.8× in commit. It runs on one core while the machine has many. On
compute-heavy workloads (Iris stages, where tiles do real arithmetic) the ratio reverses, and the
instrumentation sits idle-waiting on the program's critical path anyway.

Separately, the program and the instrumentation share an address space. A buggy or crashing tile
can corrupt the commitment builder's or the audit's state. It could also write the audit handoff
itself, bypassing the events.

## Idea

Split the run into a **producer** and a **consumer** that work at the same time:

- **Producer: the user program.** It runs tiles and sequences and is the only writer of the
  `ObjectStore`. It emits a small **descriptor** per event: what was called, at which
  coordinates, which objects it read and wrote, with their commitments. It does not send object
  payloads.
- **Consumer: the instrumentation.** It turns descriptors into step records, maintains the storage
  ledger (log, index, roots), and builds the commitment, or runs the audit (verifier, trace tree,
  `SourceTracker`, window, evidence). It reads object bytes from the program's store, **read-only**,
  only when it needs them: selection witnesses, derivation bases, list metadata, captured window I/O.

What makes this tractable: **objects are write-once and never deleted** (`ObjectStore::put`
asserts on a second write, and nothing removes). An append-only region with a published length
is enough to share them. The writer appends, then publishes; readers see only published data. No
locks are needed.

## Design

### What travels in a descriptor

Today's `TraceEvent` carries `FnCallRecord { fn_name, input, output, draft_transition_witness,
recur_control, recur_state }`.
- `output` is the heavy part: `FnOutput { data, raster: RasterPayload { bytes, index_bytes,
  root_hash }, derived }`, the object's postcard bytes and its full raster payload.
- `input` (`FnInput { data, values, args, storage }`) carries inline argument values and storage
  bindings (coordinates, commitment, selector, selection).

A descriptor keeps every field **except the output's bytes**. In their place it carries the stored
object's reference (coordinates and commitment). The commitment is the raster root the recorder
uses today (`internal_object_commitment`). Inline inputs, selections, draft operations and
recursion control stay in the descriptor: they are small, and they are not in the store.

### What is shared, and how

The `ObjectStore` becomes an **append-only byte region**:
- object records (postcard bytes, raster bytes, index bytes);
- derived objects as offset ranges into earlier records;
- a coordinates → record table.

The consumer maps it read-only. In-thread (stage 0) the region is ordinary memory behind `Arc`s.
Across processes (stage 3) it is a file-backed mapping (`memmap2`, already a `raster-runtime`
dependency), which works on Linux, macOS and Windows.

Parsed raster indexes can't be shared as Rust structures. Each side parses what it reads and
caches it. If that cost shows up in measurements, the index can be stored in a directly readable
layout instead.

### Ordering, backpressure, stopping

- **Ordering.** One producer and one consumer on one ordered channel, so the consumer sees events
  in program order. Because objects are published before their descriptor is sent, every
  referenced object is already readable when its descriptor arrives.
- **Backpressure.** The channel is bounded. When the consumer falls behind, the producer blocks.
  That caps memory for in-flight descriptors.
- **Early stop (audit).** The consumer finds a divergence after the program has already moved on,
  by at most the channel's capacity. The consumer writes the handoff and stops the program (exit
  86 semantics move to the consumer). The program's extra work is wasted, not wrong.
- **Failure.** A program panic: the consumer publishes no commitment and no handoff, as today. A
  consumer failure stops the program. The CLI's handoff validation stays the final gate.

## Stages

Each stage stands on its own and is measured before the next.

| Stage | What | Gain | Cost |
| --- | --- | --- | --- |
| **0. Measure the ceiling** | time the program and the instrumentation separately (they already are, see §Problem) and estimate `max` against `sum` for hello-tiles 20k, an Iris stage and the chain example | decides whether stage 1 is worth it | none |
| **1. Worker thread** | the publisher runs on a second thread fed by a bounded channel. The `ObjectStore` becomes readable from another thread: immutable objects behind `Arc`, an append-only concurrent table. Events still carry payloads | most of the parallelism | moderate; no new storage format |
| **2. Descriptor events** | events carry object references instead of output bytes; the consumer reads bytes from the store | removes payload copies through the channel | moderate; event format change |
| **3. Shared, file-backed store** | the `ObjectStore` as an append-only mapped region | the OS pages object bytes; could replace Disk mode and the SQLite archive | **large**: storage rewrite (`storage.rs`, `backing.rs`, ~3,600 lines) |
| **4. Separate process** | the CLI itself becomes the consumer, reading the region and a descriptor ring | isolation; one code path for live and offline (§Unification) | moderate once stage 3 exists |

### Unification (stage 4)

If the store is a file and the descriptors are a log, a **saved trace** is those two files. Offline
paths (plain `run` replay, `trace commit`, `trace audit`, `--fraud-step`, chain stage re-runs)
would then run the **same** consumer code as live runs, reading files instead of a live mapping.
That would retire:
- the separate trace-file format and its decode-and-replay;
- `WorkingStorage::Disk`, the `Archive` and its SQLite index;
- the second recorder construction for offline replay.

## Expected effect (to be measured)

- **Wall time:** from *program + instrumentation* towards *the larger of the two*, plus channel
  overhead. hello-tiles 20k audit: from ~6.2 s towards ~4.5 s (−25%), bounded by the consumer.
  Compute-heavy stages: towards the program's own time. The consumer could itself be split into
  stages, for example trace side (verifier, trace tree, `SourceTracker`) and storage side (ledger),
  using more cores.
- **Memory:** object bytes once (unchanged since `b201b0a`), no payload copies in flight (stage 2),
  and pageable by the OS (stage 3).
- **Unchanged:** commitments, evidence, guest, image ids. The consumer computes exactly what the
  in-process recorder computes, from the same events in the same order. Acceptance is the existing
  byte-identity test (`live_commitment_cli`) and canonical handoff equality on real runs.

## What it does not change

**It doesn't make events trustworthy.** In every design, including main's trace file, the program
produces the event stream, and a malicious program can lie in it. Separating the processes
protects the *instrumentation's state and verdict* from the program's memory. Soundness still
comes from the guest re-proving the fault window.

## Stage 0 results (2026-11-06)

**Method.** A bench-only probe in `raster-runtime` (`~/.cache/raster-mem-bench`, commit `eca5aae`).
It measures, inside the user program's process, from `raster::init()` to the end of
`raster::finish()`:
- the time spent inside `publisher.publish` and `publisher.finish` (the **consumer**);
- the rest (the **producer**: the program, its macros and runtime, building the events);
- the event count and the bytes events carry: `FnOutput` data plus raster payload plus derived
  delta, and `FnInput.data`.

It excludes CLI work (build check, spawn). Audits ran against honest commitments, so they execute
the whole program. Two runs each after a warm build; both shown.

| workload | mode | in-process total | producer | consumer | ceiling `max(p, c)` | ceiling vs total |
| --- | --- | --- | --- | --- | --- | --- |
| hello-tiles 20k (210,091 events) | unauthenticated | 1.76 / 1.90 s | all | — | — | — |
| | plain (trace file) | 2.58 / 2.52 s | 2.19 / 2.15 s | 0.38 s | 2.19 s | −15% |
| | live commit | 4.85 / 5.08 s | 2.35 / 2.42 s | 2.51 / 2.65 s | ~2.6 s | **−45%** |
| | live audit | 6.33 / 6.34 s | 2.43 / 2.48 s | 3.90 / 3.87 s | ~3.9 s | **−38%** |
| Iris `prefill_range_l0`, tiny model (2,043 events) | unauthenticated | 0.14 / 0.13 s | all | — | — | — |
| | live commit | 0.26 / 0.25 s | 0.20 / 0.19 s | 0.064 / 0.062 s | 0.19 s | −25% |
| | live audit | 0.31 / 0.31 s | 0.19 / 0.19 s | 0.117 / 0.119 s | 0.19 s | −38% |
| chain example (3 stages, 4–14 events each) | live commit per stage | 36–42 ms | 36–42 ms | 0.1–0.2 ms | — | ~0% |

Event payloads, per run:

| workload | output bytes in events | `FnInput.data` in events |
| --- | --- | --- |
| hello-tiles 20k | 21.2 MB | 12.8 MB |
| Iris `prefill_range_l0` | 1.6 MB | **26.7 MB** |

**What this shows:**
1. **On a step-heavy run, the pipeline's ceiling is large.** On hello-tiles the consumer is as
   large as the producer (commit) or larger (audit), so overlapping them could cut in-process
   time by 38–45%. Because the consumer is the larger side in audit, the gain is capped by it.
   Making the consumer cheaper (the coordinate index's 6–7 µs per write, for example) or
   splitting it into two stages would raise the ceiling further. The two levers compound rather
   than compete.
2. **On the tiny Iris stage it's producer-bound** (consumer 33–38% of the total), so a pipeline
   would hide the consumer almost entirely. The absolute times are small. A real-model Iris stage,
   where tiles do real arithmetic, would be even more producer-bound. Those weights aren't on this
   machine, so it isn't measured here.
3. **On small stages there's nothing to gain.** The chain example's stages spend ~40 ms on fixed
   start-up work (mostly building the CFS in the publisher's constructor) and ~0.2 ms in the
   consumer. For such runs the CLI's own fixed costs, the build check and the spawn, matter more
   than either.
4. **Descriptors must reference inputs too, not only outputs** (correction to §Design). On Iris,
   `FnInput.data`, the tile's materialized inputs, is 26.7 MB against 1.6 MB of outputs. Most of it
   is values selected from stored objects. A descriptor that carried it would still move most of
   the bytes. Inputs read from storage should travel as their selections (reference + selector),
   which the consumer can resolve from the shared store. Only genuinely inline values stay in the
   descriptor.
5. **The plain-run producer is cheaper than the commit/audit producer** (2.15–2.19 s vs
   2.35–2.48 s). The difference is runtime work done only in live modes, beyond the publisher:
   storage roots and draft authentication. It isn't counted as consumer here, so the ceiling
   figures slightly understate what a full split could reach.

**Channel overhead (estimated, not measured):** 210,091 handoffs at a few hundred nanoseconds each
for a bounded single-producer queue is ~40–100 ms. That's under 2% of the hello-tiles audit and
doesn't change the conclusion. Stage 1 should measure it.

**Decision input:** stage 1 (a worker thread) is worth building for step-heavy workloads: up to
~40% off live commit and audit, with no storage rewrite. Its size on real Iris workloads needs a
real-model measurement first. Reducing consumer cost should continue in parallel; on hello-tiles
it is the bound.

## Stage 1 results (2026-11-06)

**What landed:**
- **`publishers::worker::WorkerPublisher`** owns the run's publisher on a thread named
  `raster-instrumentation`, with a 64 MiB stack. It's fed in program order through a bounded
  `mpsc::sync_channel` (`RASTER_INSTRUMENTATION_QUEUE`, default 1024 events). A publisher panic is
  re-raised on the program's thread at its next event or at `finish`. `finish` drains the queue and
  joins the thread. Audit's divergence exit (86) now happens on the worker thread and ends the
  process as before.
- **`tracing::init` installs every publisher behind it** (trace file, live commit, live audit),
  except when profiling or selective capture is active: their state is thread-local on the program's
  thread. `RASTER_INSTRUMENTATION_THREAD=0` keeps the publisher inline.
- **The program's `ObjectStore` shares its object maps** (`Arc<RwLock<…>>`). `shared_view()` hands
  the same maps to the ledger, and `ObjectSource::Program` holds such a view instead of reading
  `THREAD_STORAGE` per call.
- **A pitfall found on the way:** entering `main`'s root sequence replaced the program's
  `ObjectStore` (`reset_thread_storage`) *after* `init` had taken the view, so the view pointed at a
  discarded store and the first read failed ("Missing storage object at coordinates []"). The reset
  now empties the store in place (`ObjectStore::reset`), keeping its shared maps. A store read
  through a view must never be replaced, only reset.

**Measured**, running the child process directly, the same binary with the worker on and off,
interleaved, four runs each:

| workload | mode | worker off | worker on | change |
| --- | --- | --- | --- | --- |
| hello-tiles 20k | live commit | 4.45–4.59 s | 3.19–3.27 s | **−28%** |
| | live audit, honest | 5.74–5.82 s | 3.97–4.17 s | **−30%** |
| | live audit, late fault (exit 86) | 5.73–5.83 s | 4.01–4.17 s | **−29%** |
| Iris `prefill_range_l0` (tiny model) | live commit | 0.24 s | 0.20–0.21 s | −13% |
| | live audit, honest | 0.28–0.30 s | 0.20–0.21 s | **−28%** |

- **Output is unchanged.** `commit.bin` and honest handoffs are byte-identical with the worker on
  and off, on both workloads. The late-fault handoff is canonically equal to the worker-off run and
  to the original pre-optimization code. `live_commitment_cli` passes with the worker as the
  default.
- **Against the stage 0 ceiling** (−45% commit, −38% audit on hello-tiles), stage 1 reaches about
  two thirds. The cost is visible in CPU time: commit uses 5.6–5.7 s of CPU against 4.5 s inline,
  audit 6.6–6.9 s against 5.7–5.8 s. Most of that is the two threads sharing objects across cores
  plus the channel. On the tiny Iris stage both modes land at ~0.20 s, which is the producer's own
  time (0.19 s in stage 0): the instrumentation is hidden.
- **Memory:** +3–4 MB peak (the queue of in-flight events and the second stack).

## Consumer profile after stage 1 (2026-11-06)

Phase timers on the consumer (bench-only, `~/.cache/raster-mem-bench`), hello-tiles 20k, worker on:

- **The consumer is saturated.** Commit: busy 2.79 s of 3.09 s, idle 0.27 s. Audit: busy 3.68 s of
  3.81 s, idle 0.10 s. The program spends 0.54 s (commit) and 1.30 s (audit) blocked on a full
  queue. Consumer cost is now the wall-clock bound.
- **Where the consumer's time goes** (audit): `record` 1.86 s, of which the storage append is
  0.67 s (journaled index insert 0.42 s, root 0.18 s). The trace-tree append is 0.61 s (~32
  SHA-256 per step, inherent). Capturing window I/O is 0.37 s, source witnesses 0.19 s, the step
  hash 0.12 s. Commit: `record` 1.69 s, commitment builder 1.03 s (trace-tree root 0.64 s, hash
  0.29 s).
- **The event's own lifecycle is the largest avoidable cost inside `record`:**
  - **Two full copies of every event.** `record` matches on `event.clone()`, and the witness store
    gets another `event.clone()`, which in commit mode it discards unused. 0.10 + 0.10 s, plus
    freeing them.
  - **Cross-thread frees.** The program allocates each event and the worker frees it. With the
    worker on, the gap between `record` measured from outside and its body is 0.41 s; with it off,
    0.07 s. About 0.34 s per run, ~1.6 µs per event, is glibc freeing memory another thread
    allocated.
- **Per event type:** `TileExec` 80,043 × 12.2 µs (storage append included), recur-tile iterations
  50,002 × 5.2 µs, recur-sequence iteration start and end 40,000 × 3.6 µs and 2.3 µs.

**Cheap fixes this points to:**
1. Match on `&event` and **move** the event into the witness store at the end, only when the store
   keeps it. That's zero copies instead of two.
2. Don't free events on the worker. Either hand each consumed event back to the program's thread to
   drop (the program has slack: it waits on the queue anyway), or use an allocator built for
   cross-thread frees. The first needs no new dependency.

Together these are ~0.5–0.6 s of a ~3.1 s commit consumer (~15–20%). After them, the remaining cost
is real work: storage append, the trace tree, input commitments, draft bookkeeping. The next step
is then the consumer split (recorder stage and trace stage).

**Done, as built:**
1. `TraceRecorder::record` and `StepWitnessStore::insert` take `&TraceEvent`. The store already
   copied each field it keeps, so moving the event into it would have saved nothing. Recording
   copies no event, and the caller keeps it.
2. `Publisher::publish_returning` hands the event back once it is recorded. The commitment and
   audit publishers implement it. `WorkerPublisher` sends returned events back over a second,
   unbounded channel, which the program drains each time it sends an event and once more at
   `finish`, so each event is freed on the thread that allocated it.

Measured on hello-tiles 20k, five interleaved runs each, median, against the stage 1 binary:

| | stage 1 | + fixes |
|---|---|---|
| commit wall / CPU | 2.96 s / 5.23 s | 2.37 s / 4.24 s (−20% / −19%) |
| audit wall / CPU | 3.75 s / 6.23 s | 3.18 s / 5.42 s (−15% / −13%) |
| peak RSS | 150 / 167 MB | 152 / 170 MB |

`commit.bin` and the honest-audit handoff are byte-identical. The divergence handoff matches on
canonical comparison (128 window steps, 128 io entries, 96 source witnesses).

## Risks and open questions

- **The storage rewrite (stage 3)** is the main cost and risk. Today's store is heap structures:
  `BTreeMap<_, Arc<StoredObject>>`, piece tables pointing at buffers, a `OnceLock` parsed-index
  cache, a referenced object resolved through files. All of that needs a byte layout, plus a
  format version shared by runtime and CLI. They must already be deployed together.
- **The descriptor format** must cover every field the recorder reads today and track the macros.
  The byte-identity test covers it.
- **Re-entrancy disappears** (in-process the recorder borrows `THREAD_STORAGE`). Ordering moves
  to the channel instead.
- **Platform details.** File-backed mappings and cross-process atomics behave the same on the
  three platforms, but `memfd`-style anonymous sharing is Linux-only. Use file-backed mappings.
- **Debuggability.** Two cooperating processes are harder to debug than one. Stage 1's thread
  version keeps one process, with most of the speed benefit, if stages 3–4 are not pursued.
- **Whether parallelism is the right next lever at all.** Stage 0 may show that making the
  consumer cheaper, for example the coordinate index at 6–7 µs per write, gains as much on one
  core. Both can be done; stage 0 decides the order.
