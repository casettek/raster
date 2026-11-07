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
  `mpsc::sync_channel` (`RASTER_INSTRUMENTATION_QUEUE`, default 1024 events, 64 since stage 2). A publisher panic is
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

## Profile after the cheap fixes (2026-11-06)

The same phase timers, on `0444a98`, hello-tiles 20k, two runs each (they agree within 2%). The
timers add 7–12% to wall time, so read the split, not the absolute times. "Own work" is the
program thread's time outside `send`; "blocked" is `send` minus freeing returned events.

| | commit | audit |
| --- | --- | --- |
| program thread: own work | 2.19 s | 2.43 s |
| program thread: freeing returned events | 0.14 s | 0.18 s |
| program thread: blocked on a full queue | 0.21 s | 0.96 s |
| worker: busy / idle | 2.03 s / 0.40 s | 3.21 s / 0.24 s |

- **Commit is now about balanced.** Each side has ~2.0–2.3 s of work. Making the consumer cheaper
  saves CPU, but at most ~0.2 s of wall time. What remains is the producer: ~0.4 s of live-mode
  runtime work over the unauthenticated program (stage 0, finding 5), and the 0.14 s of frees,
  which now sit on the program's side.
- **Audit is still consumer-bound, by ~0.8 s.** The worker's 3.21 s:
  - `record` 1.41 s: storage append 0.68 s (journaled index insert 0.44 s, root 0.18 s), input
    commitments 0.21 s, exec steps 0.19 s, recur draft 0.14 s, witness insert 0.12 s;
  - trace side 0.81 s: trace-tree append 0.62 s, step hash 0.13 s, `push_root` 0.04 s;
  - source witnesses 0.19 s and window I/O capture 0.39 s;
  - ~0.4 s outside the timed calls (recur-progress clone, window entries, the timers themselves).
- **What would close the audit gap:**
  - **Splitting the consumer.** A second stage takes the step records and runs the step hash,
    the trace tree, `push_root` and the source witnesses (~1.0 s). That leaves ~2.2 s on the
    recorder, below the program's own work, so audit would become producer-bound like commit.
    The hard part is the divergence: it is detected by the second stage, which runs behind the
    recorder. So storage must still rewind to the window's start, and the window's entries
    (captured I/O from the first stage, source witnesses from the second) must be paired up by
    step.
  - **A cheaper storage append** (the journaled index insert is 7.4 µs per write). Halving the
    append would close about half the gap, without a new thread.

## Producer profile: what stage 2 can remove (2026-11-06)

Bench-only timers around each part of the code `#[tile]` generates, and around the runtime calls it
makes, on `0444a98`, program thread only. Timers add ~5–10%. `data` is `FnInput.data`, the
arguments serialized once more for the event.

| program thread | hello-tiles 20k, unauth | hello-tiles 20k, commit | Iris tiny, unauth | Iris tiny, commit |
| --- | --- | --- | --- | --- |
| total | 1.83 s | 2.72 s | 132 ms | 209 ms |
| inside tile wrappers | 0.78 s | 1.21 s | 50 ms | 59 ms |
| · inputs read from storage | 0.41 s | 0.47 s | 1 ms | 7 ms |
| · output raster encoding (store needs it) | 0.12 s | 0.15 s | 3 ms | 3 ms |
| · **event input `data`** | **0.04 s** | **0.04 s** | **28 ms** | **30 ms** |
| · event input, rest (values, args, bindings) | 0.06 s | 0.07 s | 4 ms | 2 ms |
| handing events to the worker (all events, incl. freeing) | — | 0.31 s (0.16 s freeing) | — | 4 ms (3 ms freeing) |
| outside tile wrappers (sequences, recur sites, startup) | 1.05 s | 1.51 s | 82 ms | 150 ms |

- **Stage 2's payoff depends on the workload.** On the tiny Iris stage, `data` alone is 15% of a
  commit's program time (22% of an unauthenticated run). The run is producer-bound (the worker
  idles 107 of 170 ms), so that time is wall time. On hello-tiles it's ~0.04 s, plus some of the
  0.16 s of freeing: under 5%. The output-side copy (the waiting-slot copy) is 5 ms on hello-tiles,
  negligible.
- **An unauthenticated run builds every event and discards it.** `publish_trace_event` is called
  without a publisher, so the Iris unauthenticated run spends 22% of its time on `data` that
  nothing reads. Not building events when nothing consumes them is a separate, cheaper fix. It must
  still feed the tile census, which needs only the tile name.
- **The largest live-mode cost on the program side is not in events.** Outside tile wrappers, live
  commit adds 0.46 s on hello-tiles and 68 ms on Iris over the unauthenticated run. That's
  authenticated work in sequence and recur-site handling, and it's not yet broken down. Inside
  wrappers, the user function itself takes 0.13 s in commit against 0.02 s unauthenticated on
  hello-tiles, which is unexplained (perhaps the worker competing for cache and memory).

**Correction: `data` is committed to.** The step's `input_commitment` is SHA-256 of `data`
(`raster-runtime/src/tracing/commitment.rs`, `From<&FnInput>`), next to `input_source_commitment`
over the values and bindings. A descriptor can't simply drop it: something must still produce that
hash. The raster bytes in the store are a tree encoding, not postcard, so they can't stand in.

### Real model: Gemma 4 E2B, `prefill_range_l0` (2026-11-06)

`gemma-4-E2B-it` deterministic weights (`/data/llm/gemma-4-E2B-it-detwgt`; config and tokenizer
hashes equal Iris's checked-in manifest), imported with `iris model import` into
`/data/raster-bench/model-artifacts` (143 s, 20 GB; layer 0's weights are 145 MB). Stage inputs
come from `iris claim build`, prompt "say ciao", 1 token, which generated "Ciao". Same bench
timers, two runs each (within 1%).

| | unauth | commit | audit |
| --- | --- | --- | --- |
| wall | 6.50 s | 6.74 s (+4%) | 6.86 s (+5%) |
| CPU | — | 8.35 s | 9.14 s |
| peak RSS | 253 MB | 258 MB | 314 MB |
| worker busy / idle | — | 1.65 s / 5.03 s | 2.35 s / 4.42 s |

Program thread (commit; inside tile wrappers, unauthenticated is within 2% on every line):
- inside tile wrappers, 3.23 s:
  - **`data` serialization: 2.10 s, 31% of the run;**
  - inline values 0.26 s;
  - draft capture 0.43 s;
  - output serialization 0.16 s;
  - **the tile bodies themselves: 0.22 s;**
- handing events to the worker: 0.02 s;
- outside tile wrappers (startup, loading inputs, recur-site driving): 3.48 s (3.25 s unauthenticated),
  not broken down.

Worker (commit): `record` 1.58 s, almost all in `exec_step`: SHA-256 of `data` ~1.07 s and
`input_source_commitment` 0.48 s (it clones and serializes the inline values to hash them).

What this shows:
- **Stage 1 hides the instrumentation on real workloads.** The worker idles 75% of the run;
  commit and audit cost 4–5% over unauthenticated.
- **Building `data` is the single largest cost on the program**, in every mode, an order of
  magnitude more than the tile bodies. In an unauthenticated run nothing reads it, so not building
  events there would cut ~37% (2.4 s with the inline values).
- **In live modes the protocol needs SHA-256 of `data` for every step**, so a descriptor needs the
  bytes produced somewhere. The options:
  - serialize into the hasher on the program and send 32 bytes: saves the buffer and the copy,
    but moves ~1 s of hashing onto the program, which is the bound;
  - send the argument values instead (a clone) and let the idle worker serialize and hash: needs
    `Clone + Send + 'static` arguments, and cloning must turn out cheaper than encoding;
  - change what `input_commitment` commits to, for example the selection commitments the step
    already carries. That removes the serialization altogether, but it's a protocol change (guest,
    image ids).

**Done: no tile events without a publisher.** `raster_runtime::trace_events_consumed()` says
whether a publisher is installed. Without one, the generated tile wrapper skips everything it does
only for the event: serializing the arguments and the output, the output's raster payload, and
the record itself. It calls `note_untraced_tile` instead, which keeps the tile census exact. The
store doesn't depend on the skipped payload: with nothing stashed, `store_execution_output_value`
encodes the value itself. Sequence and recur-site events are unchanged.

| unauthenticated run | before | after |
| --- | --- | --- |
| hello-tiles 20k (5 runs each) | 1.75–1.83 s | 1.52–1.57 s (−14%) |
| Gemma 4 E2B `prefill_range_l0` (3 runs) | 6.49–6.51 s | 3.91–3.96 s (−40%) |

Live commits are byte-identical, on both workloads. The unauthenticated tile census is identical to
before. Tile image ids are unchanged; the six example `Raster.lock` files changed only their
`guest_inputs` fingerprint, because `raster` and `raster-macros` sources changed.

### Real model: where the time outside tile wrappers goes (2026-11-06)

Timers on the recur drivers and on the storage read path, unauthenticated run with the fix above
(3.9–4.0 s). `call_recur!(tile = mac_weight_page, input = w_q, …)` walks a weight matrix's
`List<BytesPage>` page by page: 8,832 page reads averaging ~210 KB, 1.88 GB in all, which is the
145 MB layer read once per prompt token. (An earlier draft said 16 KB pages read once each; it
divided the layer by the number of reads.)

| per weight page, as the driver builds the iteration's input | total | per page |
| --- | --- | --- |
| reading and decoding the page (`into_auth_value`) | **2.93 s (74% of the run)** | 331 µs |
| · fetching it (`ReferencedObject::select`) | 1.31 s | 146 µs |
| · · copy the page out of the data file and SHA-256 it (`selected_payload`) | 0.90 s | 101 µs |
| · · build its tree value, a second copy (`tree_value_from_raster_location`) | 0.20 s | 22 µs |
| · · resolve the source and verify its commitment | 0.11 s | 12 µs |
| · · re-check the schema's page sizes, on every page (`check_schema_page_sizes`) | 0.09 s | 10 µs |
| · decoding the tree into `BytesPage` (`typed_value_from_tree`) | ~1.6 s | ~190 µs |
| finding the page in the list (`select_item`), re-materializing the extra arguments | < 0.01 s | — |

The decode line is the difference. The decode timer itself reads 1.99 s, but it also covers 79
selections that aren't pages.

- **The decode is the largest single cost:**
  - `TreeValue::BytesPage` hands its bytes to serde as `PageWireField::Bytes(bytes.to_vec())`, a
    third copy;
  - `OwnedU8SeqDeserializer` then feeds them in one `u8` at a time, because `BytesPageWire.bytes`
    is a `Vec<u8>`, which serde fills element by element.

  Handing the buffer over whole would remove most of it. Decoding the field as bytes
  (`serde_bytes`, or a byte-buffer visitor) would do that, and postcard encodes a byte sequence and
  a byte string identically, so the wire format stays the same. That type lives in `raster-core`
  and is compiled into guests, so it moves image ids.
- **The page hash is computed in every mode.** `selected_hash` (SHA-256 of the page) belongs to
  the selection commitment, which only an authenticated run records. An unauthenticated run
  computes it and drops it.
- **The schema's page-size check runs on every page.** It could run once per source.
- Together, page reading and the argument serialization of the live modes (above) are what the
  program spends its time on. The tile bodies take 0.22 s of the run.

**Done: page bytes decoded whole (2026-11-07).** `BytesPageWire.bytes` is read with
`#[serde(with = "serde_bytes")]` (`raster-core/src/collections.rs`). The tree's deserializer
already moves the buffer in one piece for that request (`OwnedU8SeqDeserializer::deserialize_byte_buf`),
and `serde_bytes` was already a dependency. A new test,
`bytes_page_wire_is_unchanged_by_reading_bytes_whole`, pins the encoding to that of the plain
`Vec<u8>` struct it replaced.

| Gemma 4 E2B `prefill_range_l0` | before | after |
| --- | --- | --- |
| unauthenticated (3 runs) | 3.9–4.0 s | 2.0–2.1 s (−48%) |
| commit (2 runs) | 6.7 s | 4.8–5.0 s (−27%) |
| audit | 6.9 s | 4.8 s (−30%) |
| decoding the pages | ~2.0 s | 0.1 s |

- **Unchanged:** the real-model `commit.bin` (byte-identical), the audit verdict (honest), the
  transition and defence guest ids (checked after their build script reran), and every tile image
  id of the six example programs, which don't use `BytesPage`. Their `Raster.lock` files change
  only `guest_inputs`.
- **Iris must re-lock.** Every stage's `guest_inputs` changes, as it already did with the
  unauthenticated-run fix above. On `prefill-range` all 33 tile image ids move, measured by locking
  the stage with and without the attribute. So claims and commitments made under the old locks
  need the Raster version they were made with.
- **Not yet measured:** whether guests also get cheaper. A guest decodes its page from postcard
  through the same `Deserialize` impl, so replaying page tiles should cost fewer zkVM cycles.

**Done: page reads in unauthenticated runs (2026-11-07).** Timing the copy and the hash
separately showed the copy is 5–6 µs per page. SHA-256 runs at full hardware speed (2.3 GB/s,
the same as OpenSSL on this machine), but it hashed 1.88 GB per run. Two changes, both in
`raster-runtime`, so no guest and no lock is affected:
- `selected_payload_from_raster_location` computes `selected_hash` only when the run is
  authenticated. Nothing in an unauthenticated run reads it: the selection commitment is copied
  into trace data only, and `verify_bound_index_bindings` runs in guests and tests.
- `ReferencedSource::check_page_sizes` runs `check_schema_page_sizes` once per source, not on
  every read. Every clone of the source shares the result. The source's content is fixed for the
  run: the resolver hands out one cached copy, checked against the source's commitment before
  each read.

| Gemma 4 E2B `prefill_range_l0` | before | after |
| --- | --- | --- |
| unauthenticated (3 runs) | 2.0–2.1 s | 1.08–1.10 s (−47%) |
| commit (2 runs) | 4.8–5.0 s | 4.63–4.70 s (the page-size check only) |

`commit.bin` is byte-identical. All 668 tests pass, with no `Raster.lock` change. What's left of a
page read in an unauthenticated run: a second copy into the tree value (18 µs), resolving the
source (11 µs, it re-reads the input and manifest entries on each call) and the decode (11 µs).

**Done: stage 2, option b — the worker encodes `data` (2026-11-07).** The tile wrapper clones each
argument and sends the clones with the event; the publisher encodes them into `FnInput::data`, on
the worker thread when there is one. A Gemma `mac_weight_page` input (a 215 KB page, a 6 KB row, a
`u32`) clones in 3.2 µs and encodes in 230 µs: postcard writes the page's `&[u8]` one element at a
time.
- `raster-runtime/src/tracing/deferred_input.rs`: `DeferredInput` holds one part per parameter.
  Postcard encodes a tuple as its elements in sequence, so the parts concatenated are the bytes the
  wrapper encoded before; a part that fails to encode empties the whole input, as
  `unwrap_or_default` did. `DeferArg` picks the part by auto-reference: a boxed clone for a
  `Serialize + Clone + Send + 'static` argument, else encoded on the spot. Drafts still encode their
  replay handle in the wrapper.
- `Publisher::publish_deferred` encodes in place by default. `WorkerPublisher` encodes on its
  thread, frees the encoded bytes there, and hands the clones back with the event.
- The profiling wrapper still encodes in place: it reports the input's size, and its publisher
  runs on the program's thread.
- `raster-core` is unchanged. Tile image ids are unchanged; the six example `Raster.lock` files
  change only `guest_inputs`.

A/B on two sibling pairs on `/data/raster-bench/ab` (base: `93fe38f`, with the page-read fix above;
new: plus this, queue 64), release builds, no timers, five interleaved runs each, median (range).
Measured on an otherwise idle machine; a first run alongside a game was discarded.

| | base | new |
| --- | --- | --- |
| Gemma `prefill_range_l0`, unauthenticated | 1.05 s (1.04–1.07) | 1.05 s (1.04–1.07) |
| Gemma, live commit | 4.56 s (4.52–4.72), 257 MB | 3.64 s (3.61–3.67), 268 MB (−20%) |
| Gemma, live audit | 4.52 s (4.49–4.67), 313 MB | 4.28 s (4.25–4.34), 324 MB (−5%) |
| hello-tiles 20k, live commit | 2.48 s (2.44–2.56), 151 MB | 2.47 s (2.44–2.51), 146 MB |
| hello-tiles 20k, live audit | 3.35 s (3.31–3.50), 168 MB | 3.32 s (3.26–3.38), 163 MB |

All commitments are byte-identical, and so are the honest-audit handoffs (10 of each per
workload). hello-tiles is unchanged within the noise; the +3% on its commit in the discarded
run did not reproduce. Why the Gemma audit gains less than the commit is not measured.

**Queue length.** The worker is now the slower side on Gemma, so the queue stays full, and each
queued event holds a weight page. At the old default of 1024 events the commit ran as fast
(3.52–3.67 s) but peaked at 415 MB, against 268 MB at 64. The default is now 64; hello-tiles runs
the same with it.

**Done: pages encode as a byte buffer (2026-11-07).** After option b the worker spent its time
writing pages one byte at a time. `BytesPageWireRef.bytes` now serializes with `serde_bytes`, as
`806e8e2` did for decoding. A 215 KB page encodes in 3.4 µs instead of 230 µs; hashing it
(92 µs) is now most of what's left of an input.
- The encoding is unchanged. postcard and bincode write a byte buffer and a `u8` sequence alike, as
  a length and the raw bytes, and JSON writes both as an array of numbers. A new test checks all
  three, with `postcard_wire_is_pinned` still passing. `risc0_zkvm::serde` would differ, but it
  encodes only journals, which hold no pages. `bytes_page_parts` already accepted
  `serialize_bytes`, and the tree serializer intercepts pages by name before reaching it.
- No example tile id moved (the six locks change only `guest_inputs`), and the transition,
  authorization, chain-fraud and defence guest ids are unchanged (`methods.rs` was not rewritten).

Three-way A/B, as above (a third pair: option b plus this), five interleaved runs each, median:

| | base `93fe38f` | + option b | + byte-buffer pages |
| --- | --- | --- | --- |
| Gemma `prefill_range_l0`, unauthenticated | 1.04 s | 1.04 s | 0.77 s (−26%) |
| Gemma, live commit | 4.49 s, 257 MB | 3.60 s, 268 MB | 1.81 s, 257 MB (−60%) |
| Gemma, live audit | 4.51 s, 313 MB | 4.25 s, 325 MB | 2.34 s, 325 MB (−48%) |
| hello-tiles 20k, live commit | 2.42 s | 2.44 s | 2.42 s |
| hello-tiles 20k, live audit | 3.36 s | 3.31 s | 3.33 s |

Ranges were within ±0.1 s of the median except one base audit at 4.71 s. All commitments and
honest-audit handoffs are byte-identical (15 of each per workload). The unauthenticated run gains
too, so the program also encodes pages somewhere, probably in the recur state carrying the
accumulator page; not broken down.

**Profile after byte-buffer pages (2026-11-07).** Timers on the current code (a fourth pair,
`ab/prof`), Gemma `prefill_range_l0`, WoW running at load 5, so read the split:
- **Commit is bound by the program thread**: it waits on the queue 15 ms in 1.86 s. Of its time,
  0.80 s is SHA-256 of each selected weight page (the selection commitment, authenticated runs
  only); the rest is about what an unauthenticated run does. The worker hashes `data` (0.90 s),
  runs `input_source_commitment` (0.39 s), and idles 0.39 s.
- **Audit is bound by the worker**: the same two, plus 0.39 s capturing window I/O for every step.
  The program waits 0.33 s on a full queue.
- Each page is hashed twice, once per thread, over different preimages (the selection hashes the
  page; `input_commitment` hashes the encoded arguments). Neither is removable without a protocol
  change.

**Done: `input_source_commitment` streams into the hasher (2026-11-07).** It hashed
`source_witness_bytes()`, which clones the values and the storage map and encodes each inline
value one element at a time: about 25 KB per Gemma step, 29 µs of encoding against 12 µs of
hashing. The recorder now serializes borrowed values straight into SHA-256 through a postcard
`Flavor`, writing inline bytes as one buffer. The bytes hashed are unchanged; the transition guest
still calls `source_witness_bytes`, and a test checks the two agree. Host only: no guest, id or
lock changes.

Seven interleaved runs each, median, with WoW running (paired comparison only):

| | byte-buffer pages | + streamed `input_source_commitment` |
| --- | --- | --- |
| Gemma, live commit | 1.86 s, CPU 3.28 s | 1.88 s, CPU 3.08 s |
| Gemma, live audit | 2.44 s, CPU 4.29 s | 2.17 s, CPU 3.96 s (−11%) |
| hello-tiles 20k, live commit | 2.55 s | 2.51 s |
| hello-tiles 20k, live audit | 3.42 s | 3.34 s |

As the profile predicted, the commit's wall time doesn't move (the program thread is its bound),
while the worker-bound audit gains. Commitments and handoffs are byte-identical (14 of each).

**Done: the audit moves a step's I/O into its window instead of copying it (2026-11-07).** Timers
on the capture (`ab/prof`) showed three copies of each step's recorded I/O. The store builds an
entry holding the encoded arguments twice (as `input_data` and inside `input_source_witness`);
`capture_step_io` cloned that entry whole, about 433 KB per Gemma step and 34.7 µs; the scope
lookup cloned the whole parent entry to read one field (1.23 µs per hello-tiles step, its largest
capture cost); and `release_live_step` then dropped the store's entry. 97.6% of Gemma's steps and
81% of hello-tiles' are released right after capture; the rest are `Start` steps, which stay.
- `TraceRecorder::take_live_step_io` captures and releases in one call: it moves the entry out of
  the store when the release would drop it, and copies it otherwise. Both use one rule,
  `releases_own_entry`. The scope lookup clones only the parent's `FnInput`. The audit calls
  `take_step_io` instead of `capture_step_io` and `release_live_step`.
- A recorder test checks the taken I/O, scope and remaining store against capture-then-release
  across a recur site's start, iterations and end.
- Fault handoffs are canonically equal before and after (`cmp_handoff`) for five faults: two in
  hello-tiles' default run (a tile and a recur iteration, 32-step windows), one deep in hello-tiles
  20k, and an early and a deep one in Gemma. An executor replay of one gives identical tile cycles.

Seven interleaved runs each, median, with WoW running (paired comparison only):

| live audit | before | after |
| --- | --- | --- |
| Gemma `prefill_range_l0` | 2.14 s, CPU 3.90 s, 323 MB | 1.92 s, CPU 3.54 s, 314 MB (−10%) |
| hello-tiles 20k | 3.40 s, CPU 5.86 s | 3.13 s, CPU 5.44 s (−8%) |

**Done: the encoded arguments are kept once per step (2026-11-07).** `StepWitnessData.input_data`
always equalled `input_source_witness.data`: every entry sets both from the same `FnInput`, or
neither. The field is gone, and `input_data()` reads the bytes from the source witness. The
entry's bincode form goes only into the run's own scratch archive (a `TempDir`), so nothing else
reads it. The five fault handoffs above are again canonically equal.

| live audit, seven interleaved runs, median, WoW running | before | after |
| --- | --- | --- |
| Gemma `prefill_range_l0` | 1.92 s, CPU 3.54 s, 314 MB | 1.90 s, CPU 3.38 s, 287 MB |
| hello-tiles 20k | 3.11 s | 3.10 s |

As expected, this is a memory change: Gemma's audit peaks 27 MB lower, now 30 MB above the commit
(257 MB); the time is unchanged, since the audit is now bound by the program thread, as the
commit is.

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
