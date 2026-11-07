# Proposal: `hash-pages-once` — commit a stored argument once, and hash it off the program thread

Status: **proposed 2026-11-07 — stage 0 measured** (§Stage 0 results). Parts A and B not
implemented. Stage 0 found a larger guest cost outside them, decoding the step's evidence byte by
byte, which is **fixed** (§Done: evidence byte fields decode whole): Gemma fault window −40% cycles.

Related:
- [`instrumentation-pipeline`](./instrumentation-pipeline.md): the profile this starts from
  (§Profile after byte-buffer pages), and the worker thread that Part B uses.
- [`program-identity`](./program-identity.md): why a step commits to its input bytes
  (`input_commitment`), so a replay proves `B(recorded input) = output`.
- [`tile-io-structural-roots`](./tile-io-structural-roots.md): the replay's `input_roots`, which
  already join each stored argument to the bytes its selection proves.
- [`paged-bytes`](./paged-bytes.md): what a `BytesPage` is and how it is selected.

## Problem

After the 2026-11-07 changes, a live commit of Gemma 4 E2B `prefill_range_l0` takes ~1.86 s and a
live audit ~1.90 s, against 0.77 s unauthenticated. Both are now bound by the **program thread**:
it waits on the worker's queue 15 ms in a commit. Of the program's ~1.0 s of extra work over an
unauthenticated run (1.86 s against 0.83 s in the same profile), **0.80 s
is SHA-256 of each selected weight page** (8,940 reads of ~215 KB). The worker, meanwhile, spends
**0.90 s hashing the same pages again**, inside `input_commitment`.

Each weight page is hashed by both host threads, and repeatedly in the guests:

| where | what is hashed | code |
| --- | --- | --- |
| host, program thread | the selected page: `selected_hash` | `raster-runtime/src/input.rs:997` |
| host, worker | all encoded arguments, page included: `input_commitment` | `recorder.rs` `exec_step`, `commitment.rs:44` |
| tile guest | its raw input, page included: the journal's `input_commitment` | macro `gen_replay_output_serialization` |
| tile guest | the decoded page's raster root: `input_roots` | macro `gen_input_roots_capture` |
| transition guest | the recorded input bytes, **twice** | `checks/io.rs:29` and `:120` |
| transition guest | the selected bytes against `selected_hash`, then the selection proof | `verify_selection_witness`, `input.rs:1468` |
| transition guest | the selected bytes' structural root, for `verify_input_roots` | `checks/io.rs:213` |

Two of the host hashes cannot simply be dropped: they have different preimages, and each feeds a
commitment the guests check. Moving the program's hash to the worker (option 3 in
`instrumentation-pipeline`) does not help on its own either: the worker would then carry
0.8 + 0.9 s of hashing and become the bound at ~2.3 s.

## Why there are two commitments

A stored argument (a weight page read by `select!`) is bound twice:
- **to storage**, by its selection: `SelectionCommitment { source_root_hash, path, selected_hash,
  … }` in the step's `FnInput.storage`, covered by `input_source_commitment`. The transition guest
  checks the selection witness's bytes against `selected_hash` and folds them up the selection
  proof to the stored object's root.
- **to the replay**, by `input_commitment = SHA-256(data)`: the tile guest commits SHA-256 of the
  exact bytes it ran on, and the transition guest compares that with the recorded input witness.

`input_roots` (tile-io-structural-roots step 2) then joins the two: the transition guest requires
every stored argument's selection to fold from the root the replay computed over the decoded
value. So for a stored argument, `input_commitment` re-binds bytes that the selection and the
input root already bind. Only inline arguments (`row`, `state`, `in_width` here, ~25 KB per step)
rely on `input_commitment` alone.

## Proposal

### Part A (protocol): a stored argument enters `input_commitment` by its root

Redefine the input commitment per argument:

```
input_commitment_v2 = SHA-256( "raster/input/v2"
                               ‖ for each parameter, in order:
                                   0x00 ‖ len ‖ raw argument bytes     (inline value, draft handle)
                                   0x01 ‖ root                         (storage-bound value) )
```

- **Tile guest.** It already decodes its arguments one by one and computes each one's raster root
  for `input_roots`. It takes each argument's raw slice while decoding (postcard encodes the tuple
  as its arguments in sequence, so `take_from_bytes` yields the slice at no extra cost) and folds
  inline slices and stored roots into the v2 hash. Which arguments are stored has to be known to
  the guest: it is the step's `FnInputValue::StorageBinding` pattern, which must become part of
  the replay input (open question 2).
- **Transition guest.** It stops hashing the input bytes. It checks the replay journal's
  `input_commitment` against the step's recorded one directly, and keeps `verify_input_roots`.
  The recorded input witness then only has to carry the inline parts.
- **Host.** The worker computes v2 from `DeferredInput`'s parts, which are already per argument:
  inline parts are hashed (~25 KB per Gemma step, not ~220 KB), and a stored part contributes its
  root (open question 1).

Two variants differ in what else changes:
- **A1 (minimal).** Only the definition above. The selection still carries `selected_hash`, and
  the transition guest still checks a consumed argument's bytes against it.
- **A2 (hash once).** In addition, a *consumed* stored argument is proven by **reference**: its
  selection witness carries the subtree root instead of bytes (the existing
  `SelectionWitness::reference` and `verify_selection_reference`, today allowed only for forwarded
  bindings), and the root the replay committed in `input_roots` must equal it. The page bytes
  never enter the transition guest, the handoff stops carrying them, and `selected_hash` for such
  a binding becomes its root. The host then hashes each page exactly once, for its root.

A2 is the real "hash once". It needs a soundness argument (open question 3); A1 is the fallback.

### Part B (threading): the remaining page hash runs on the worker

Once Part A removes the worker's 0.9 s, move the program's page hash (`selected_hash` under A1,
the stored argument's root under A2) to the worker. The program would leave it unset and the
worker would fill it in before recording, as option b did for `FnInput.data`
(`deferred_input.rs`). The event would carry the selected bytes, which the read allocates anyway,
or a range into the shared source cache. This needs the program to read none of these hashes
itself (open question 4).

## Expected effect (estimates, to be measured)

Host, Gemma `prefill_range_l0` live commit, from the 2026-11-07 profile:
- **Program thread:** ~1.86 s → ~1.06 s (0.77 s unauthenticated work, ~0.1 s of tile events,
  ~0.2 s of other authenticated reads), once the page hash moves off it.
- **Worker:** today 0.90 s (`input_commitment`) + ~0.11 s (`input_source_commitment`, after
  streaming) + ~0.3 s other. After A and B: ~0.8 s page hash (or root) + ~0.1 + ~0.1 (inline
  `input_commitment`) + ~0.3 ≈ 1.3 s.
- **Wall:** ~1.86 s → ~1.3 s (−30%), now bound by the worker. The audit has the same shape, plus
  its window capture.
- Part A alone: CPU −0.8 s, wall unchanged. Part B alone: worse (worker-bound at ~2.3 s).

Guests, per Gemma weight-page step in a fault window (128-step window, `--audit --estimate`,
2026-11-07): tile 3.98M user cycles, transition 9.10M (median). How much of that is the input
hashes Part A removes (one in the tile guest, two in the transition guest, each over ~220 KB) is
**not measured**; stage 0 does that. A2 also removes the transition guest's selection-byte hash
and structural root, and the page bytes from each step's evidence.

## Stages

| stage | what | changes ids | measures |
| --- | --- | --- | --- |
| **0. Measure** | bench-only `env::cycle_count` markers around each hash in the tile and transition guests; root availability (open question 1) | no | guest cycles per hash; host cost of a page root |
| **1. Part A1** | v2 `input_commitment` in the tile guest wrapper, the transition guest and the recorder; replay input carries the binding pattern | **yes**, every tile id and the transition guest | host CPU, guest cycles, evidence size |
| **2. Part B** | the remaining page hash deferred to the worker | no | wall time, which thread is the bound |
| **3. Part A2** | consumed stored arguments by reference | **yes** | guest cycles, evidence size |

Stage 2 can land after stage 1 without further protocol change. Stage 3 is decided after stage 0
and the soundness review.

## Verification

A protocol change breaks byte-identity with earlier commitments, so the checks are within a version:
- `commit.bin` and fault handoffs identical with the worker on and off
  (`RASTER_INSTRUMENTATION_THREAD=0`) and across repeated runs.
- The fault windows used for the 2026-11-07 changes (hello-tiles' default run: a tile and a recur
  iteration; hello-tiles 20k deep; Gemma early and deep), executed with `--audit --estimate`, plus
  the ignored suites (`stage_challenge_cli`, `replay_profile_cli`, `fault_estimate_cli`,
  `chain_repeat_cli`).
- Negative tests: a replay that changes one byte of an inline argument, or decodes a stored
  argument whose root differs from its selection, must be rejected by the transition guest.
- Re-lock: the six examples and all ten Iris stages; old commitments are then unauditable.

## Stage 0 results (2026-11-07)

Bench copy `/data/raster-bench/ab/p0` (the code at `2418dc1`) with `env::cycle_count` probes in
the tile guest's replay wrapper and the transition guest, printing to the guest's stderr. Fault
windows run with `--audit --estimate`; the probes add 0.3% to Gemma's cycles.

**Gemma `prefill_range_l0`, 128-step window around a weight-page fault** (exec 5157; 127
`mac_weight_page` steps), mean user cycles per step:

| transition guest, 9.13M per step | cycles | | tile guest, 3.98M per step | cycles |
| --- | --- | --- | --- | --- |
| **reading the step's input frame** | **5.83M** | | input roots (`value_root` of each argument) | 1.65M |
| selection witness (`verify_selection_witness`) | 0.95M | | tile body (the MAC) | 1.50M |
| `verify_input_roots` | 0.72M | | after the body (output, input SHA, journal) | 0.46M |
| `input_source_commitment` | 0.42M | | of which the input SHA | 0.25M |
| program frame (`PublicParams::read`) | 0.30M | | decoding the input (227 KB) | 0.34M |
| input SHA in `verify_io_witness` | 0.25M | | | |
| input SHA against the replay journal | 0.25M | | | |
| chain link, authorization, state, receipt | 0.12M | | | |

Findings:
- **SHA-256 is already accelerated in the guests**, including the `sha2` crate the tile guest and
  `selection_payload_hash` use: 227 KB costs 248k cycles (~1.1 per byte) in both guests.
- **The hashes Part A1 removes are 0.74M per step** (one in the tile guest, two in the transition
  guest) of 13.1M: about 6% of the window. A2 adds most of the selection check (0.95M) and part
  of `verify_input_roots` (0.72M).
- **Reading the input frame is 64% of a transition step**, and it is not hashing. The frame
  carries three large byte fields without `serde_bytes`, which postcard decodes one element at a
  time: `FnInput.data` in `input_source_witness` (~227 KB, a second copy of `input_witness`,
  which *is* `serde_bytes`), the inline values (`FnInputValue::Inline`, ~25 KB) and
  `SelectionWitness.bytes` (~195 KB). ~447 KB at ~13 cycles per byte is the 5.8M measured. The
  arithmetic fits; the fix is what would prove it.
- `tile.input_roots` (1.65M for ~227 KB of arguments, ~7 cycles per byte) is larger than its one
  hash per page explains. Not broken down.
- hello-tiles' default window (32 steps, small arguments) spends its transition cycles on the
  program frame and the step checks; no hash or decode stands out.

Open questions 1 and 4, answered by reading the code:
- **1. A stored argument's root is free on the host.** Every index node stores its `root_hash`
  (`RasterNode`, `raster_index.rs:45`), and `locate` returns the selected node's
  (`RasterSelectionLocation.root_hash`, `:142`). It is the root the selection proof folds from,
  and today's `verify_input_roots` already requires the replay's root to equal it. Under A2 the
  host would hash no page at all. The trade-off: it would rely on the index's node hashes instead
  of hashing the bytes it read.
- **4. The program never reads `selected_hash`.** Outside the recorder (which runs on the worker),
  runtime code only writes it (`input.rs`, `backing.rs`) and copies it: into the trace memo
  (`raster/src/input.rs:2276`), forwarded bindings (`:1567`, `:1950`, `:2399`), the recur macro
  (`recur.rs:644`) and the tile wrapper (`lib.rs:1734`). Part B must fill every copy. The worker
  can do it by key, since a selection's hash is fixed by its source root, path and kind, and the
  read that computes it precedes every forward.

### What this changes

The cheapest large guest win is not in this proposal: marking those three fields `serde_bytes`
(and storing the input bytes once in the frame) changes no commitment and no wire format
(postcard, bincode and JSON write a byte buffer and a `u8` sequence alike, as for `BytesPage`).
It does move tile and transition ids. Estimate: most of 5.8M cycles per Gemma weight-page step,
roughly 40% of the window. It should come before Part A, and Part A's guest case should be
re-measured after it.

### Done: evidence byte fields decode whole (2026-11-07)

`FnInput.data`, `FnInputValue::Inline` and `SelectionWitness.bytes` are now `serde_bytes`
(`raster-core`). The encoding is unchanged: a new test (`trace::byte_field_wire_tests`) checks
postcard, bincode and JSON against the plain `Vec<u8>` fields, decoding of older bytes, and
`source_witness_bytes`, which `input_source_commitment` hashes.
- Host: Gemma's and hello-tiles 20k's live `commit.bin` are byte-identical to before, and the five
  fault handoffs used throughout are canonically equal.
- Ids: tile ids moved in hello-tiles and the three chain-example phases (not in `planner` or
  `step`); the transition and defence guest ids moved, authorization and chain-fraud did not. The
  Iris locks need re-locking.

Same probes, same Gemma window, user cycles:

| | before | after |
| --- | --- | --- |
| transition guest (128 steps) | 1,169.2M | 502.4M (−57%) |
| tile guest (127 steps) | 506.1M | 506.1M |
| **window** | **1,675.4M** | **1,008.7M (−40%)** |
| per step: reading the input frame | 5.83M | 0.92M |
| per step: `input_source_commitment` | 0.42M | 0.13M (inline values now encode as one buffer) |

Every other probe is unchanged to the cycle. hello-tiles' default window: 20.06M → 19.99M.

With this, a Gemma weight-page step is ~3.9M transition cycles plus ~4.0M tile cycles. The three
input hashes Part A1 removes (0.74M) are now ~9% of a step; A2's further share (most of the
selection check, 0.95M, and part of `verify_input_roots`, 0.72M) roughly another 15–20%. Those
are now worth re-weighing against a protocol change. The tile guest's input roots (1.65M) are the
largest single cost left that is not the tile's own work.

## Open questions

1. **Is a stored argument's root free on the host?** A leaf's root is `selection_hash("leaf" ‖
   bytes)` (`parse_subtree_root`, `input.rs:738`). A `List` handle stores its root inline
   (`0x09`, `tree.rs:616`), but whether the raster index keeps each element's root, which a page
   would need, is not established. If it does, the host's page hash disappears under A2. If not,
   the host computes it once, on the worker (Part B).
2. **How the tile guest learns which arguments are stored.** The pattern is in the step's
   `FnInput.values`, which the transition guest has. It has to reach the tile guest without
   becoming something the prover can choose freely: the transition guest must check that the
   replay used the recorded pattern.
3. **Soundness of A2.** Today a reference witness is accepted only for a binding the step never
   consumes (`verify_selection_reference_witness`, `input.rs:1493`), because bytes are what tie a
   consumed value to its commitment. Under A2 that tie is the replay's `input_roots`, computed in
   the proven tile guest over the decoded value, and the root function is the one selections
   already use (`verify_input_roots` compares them today). The argument has to cover
   `Result`-wrapped (`Ok`) roots, `RecurInput` items (whose root is the inner value's), and
   `payload_kind`.
4. **Does the program read `selected_hash`?** Part B assumes only the instrumentation does. To be
   checked across `raster-runtime/src/input.rs` (including the `select` fallback paths) and the
   macros.
5. **Evidence format and tools.** `replay_profile` and `fault_estimate` read the recorded input
   witness; under A2 it no longer carries stored arguments' bytes, so those tools need the page
   from storage instead.

## What it does not change

- What a tile computes, or the bytes it is replayed on (`data` stays the postcard tuple).
- The selection proofs for forwarded bindings, the storage chain, and the trace leaf layout apart
  from the meaning of `input_commitment`.
- Unauthenticated runs, which compute none of these hashes.
