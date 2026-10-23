# Proposal: `transition-split` — four parts and a FRAUD WINDOW aggregator

Status: **Implemented 2026-10-22, migration steps 0–5** (§Step 0 results to §Step 5 record).
FRAUD WINDOW is the only fraud-proof pipeline: the transition chain, its per-step evidence and its
`TransitionJournal` are deleted. Rewritten 2026-10-22 from a proposal first made 2026-10-15 as
seven part guests, one per check family, plus an aggregator (§History). By then control flow and
storage had been extracted as segment guests, the transition guest proved a whole window per
execution, and the remaining families were measured. That measurement changed the split: four
parts by data source, not seven by check family. The sections up to §Migration are the design as
proposed; the step records say what was built and measured.

Related:
- [`control-flow-prover`](./control-flow-prover.md), [`storage-prover`](./storage-prover.md): two
  of the four parts, implemented as segment guests. Their journals change shape here
  (§Changes to the existing guests).
- [`transition-segments`](./transition-segments.md), [`transition-guest-trim`](./transition-guest-trim.md):
  the transition guest as it is today, and the measurement this design rests on.
- [`compact-coordinate-index`](./compact-coordinate-index.md): why storage is now small enough that
  the four parts are similar in size.
- [`chain-fraud-proof`](./chain-fraud-proof.md): the chain-fraud guest never verifies a fraud-proof
  receipt, so it is unaffected.
- [`program-identity`](./program-identity.md): `program_commitment`, which every part that reads the
  program commits and the aggregator equates.

## Problem

Today a fraud window is proven by three segment guests and one transition guest, which is the
sync point for everything:

```
tile replays ─┐
authorization ┤ receipts
control flow ─┤ (assumptions) ──▶ TRANSITION GUEST ──▶ TransitionJournal(Finished)
storage ──────┘                    trace + verdict, io, recur, drafts, entrypoint,
                                   and per-step checks of every inner guest's entry
```

Measured on `hello-tiles --fraud-step 27` and `prompt-prepare --fraud-step 796`, whole-window
mode, after `transition-guest-trim` trims 1 and 2 (`transition-guest-trim` §Where the transition
guest's cycles go gives the split before the trims):

1. **It does three unrelated jobs.** It makes the result a fraud proof (window ↔ `commit.bin`,
   trace, fingerprint, verdict). It checks what the tiles computed (io, recur, drafts,
   entrypoint). It composes the other proofs: per step it verifies the inner entry, its Merkle
   path and every field equality. Before the trims that composition was 22–23% of the guest, and
   the trace work 24–31%.
2. **It can't start until its inputs are proven,** as the host schedules it today. And it can't be
   cut into independent pieces: transition segment `k + 1` verifies segment `k`'s `Next` journal,
   so a long window is a chain again.
3. **Tile replays are on its path.** io verifies every replay receipt inside it, so all tile proofs
   must exist before it can finish.

## Goal

Split the proof of a window **by data source**. Each part reads one source of truth, proves its
slice of the window in segments of any length, and commits only *claims*: its start and end
state, and digests of the values other parts need. One guest, **FRAUD WINDOW**, verifies every
receipt, checks that the claims fit together, and commits the final journal.

```
 commit.bin ───────────▶ TRACE         trace tree, fingerprint, window ↔ commitment, verdict
 program (CFS) ────────▶ CONTROL FLOW  where steps sit, where arguments come from
 store witnesses ──────▶ STORAGE       reads, writes, roots
 tile replays ─────────▶ TRANSITION    io + recur + drafts + entrypoint: what the tiles computed
   (journal values)                      (+ the authorization journal, for entrypoint)

 every part's segment receipts ─┐
 tile replay receipts ──────────┼──▶ FRAUD WINDOW: verify, stitch, compare digests ──▶ journal
 authorization receipt ─────────┘
```

Non-goals:
- No change to any check's logic. Checks move between guests; their assertions don't change.
- No change to `StepRecord`, the trace, `commit.bin`, the fingerprint, or what a `Finished` journal
  means. A fraud proof refutes the same commitment for the same reason.
- No change to the tile replay or authorization guests.

## Design

### The four parts

| part | runs (moved from) | state across steps | reads | per-step cost today |
| --- | --- | --- | --- | --- |
| **TRACE** | `assert_window_is_commitment_slice`, `assert_opens_at_genesis`, the trace append and fingerprint accumulation (`append_to_trace`), the verdict (`reaches_window_end`), all from the transition guest | trace frontier and root, fingerprint accumulator, the window binding (refuted commitment, terminal flag, final committed root) | `InitTransition`, the commitment header and slice witness, revealed tail roots, each step's record | ≈33k (one fold and one append) |
| **CONTROL FLOW** | unchanged: the control-flow guest | trace frontier (to derive each step's root and position), expected coordinates | program, records, input-source and scope witnesses | 15–29k check |
| **STORAGE** | unchanged: the storage guest | `StorageState` (append-log frontier, index root) | records, input-source and selection witnesses, storage witnesses | ≈100k on these programs (compact index) |
| **TRANSITION** | io, recur, drafts, entrypoint, from the transition guest | recur-progress stack, entrypoint and output authorization | program, records, replay journals **as values**, I/O, input-source, selection and draft witnesses, the read witnesses for recur and `ProgramEnd`, the authorization journal | 15–40k |

TRANSITION keeps the name of today's guest because it keeps the checks that decide whether a step
was a correct *transition* of the program. It loses the trace (to TRACE) and the composition (to
FRAUD WINDOW).

### What every segment commits

Each part proves its steps in segments of any length, chosen by the host, as control flow and
storage do today. Every segment commits the same envelope:

```rust
// raster-core/src/fraud_window.rs
pub struct PartSegmentJournal {
    pub part: PartKind,                     // Trace | ControlFlow | Storage | Transition
    pub window_id: Hash32,                  // sha256(postcard(InitTransition))
    pub program_commitment: Option<Hash32>, // parts that read the program
    pub first_position: u64,
    pub step_count: u64,
    /// The record chain before and after this segment:
    /// r' = H(domain ‖ r ‖ hash_trace_item(record)) over the segment's records.
    pub records: (Hash32, Hash32),
    /// The part's own state before and after, as a digest.
    pub state: (Hash32, Hash32),
    /// One chain per value this part produces or consumes for another part,
    /// before and after this segment (§Shared values).
    pub columns: Vec<(Column, Hash32, Hash32)>,
    /// What only this part can say, on the segment that says it (§Outputs).
    pub out: Option<PartOutput>,
}
```

**Stitching instead of verifying predecessors.** A segment doesn't verify the segment before it.
It commits where it started and where it ended. FRAUD WINDOW checks that a part's segments
continue each other: `records`, `state` and every column's `out` equal the next segment's `in`,
positions are contiguous, and the first segment starts at the window's first position. So **every
segment of every part can be proven at the same time**, at any window length, and each part may
use its own segment length. This replaces today's `Next` chain.

**The opening segment** of each part reads `InitTransition` and asserts that its `state.0` is the
state that `InitTransition` implies (TRACE: the window binding and initial frontier; CONTROL FLOW:
the initial trace frontier and no expected coordinates; STORAGE: the initial store; TRANSITION:
the genesis authorization check and the recur seed, validated by advancing it as today). Chains
start from the same `window_id`-derived seed, so FRAUD WINDOW needs no part-specific knowledge of
initial states.

**The record chain** is how all four parts agree they ran on the same records in the same order:
their final `records.1` must be equal. It costs one hash per step per part, about the same as the
`hash_trace_item` every part already computes.

### Shared values: chains, not per-step entries

Today the transition guest checks each extracted guest's per-step entry, at 20–37k cycles a step
(`transition-guest-trim` §Why not extract io or recur). Here a value that crosses parts is folded
into a **column chain**, `c' = H(domain ‖ c ‖ value_i)`, by the part that produces it and,
independently, by the part that consumes it. FRAUD WINDOW compares the two final chains once. A
consumer handed a wrong value at any step ends with a different chain. The per-step cost is one
hash on each side.

The complete inventory, from reading every cross-family read in today's transition guest
(`apply_verified_step`):

| column | value at step `i` | producer (why it is true) | consumer (what reads it) |
| --- | --- | --- | --- |
| `store_before` | `StorageState` before step `i` | STORAGE: computed from checked reads and writes | TRANSITION: recur's returned-state read (`verify_returned_state`), `verify_program_end`, both against these roots |
| `selections` | `selection_witnesses_commitment` of step `i`'s map | STORAGE: the map it folded | TRANSITION: recur's item and list length, io's input roots |

That's all. Everything else a part needs is either its own state, the record (shared through the
record chain), a witness anchored to the record (the input-source witness, checked by every part
that reads it), or a receipt FRAUD WINDOW verifies:
- **CONTROL FLOW ↔ TRACE:** control flow derives each step's trace root and position from the
  shared start frontier and the record chain, so there's no `trace_root_before` column. The two
  must agree because both append the same record hashes to the same start.
- **Expected coordinates:** internal to CONTROL FLOW. Nothing else reads them.
- **The record's output commitment:** STORAGE writes it, TRANSITION ties it to the replay output
  root. Both read it from the record, so the record chain binds them.
- **Recur ↔ drafts:** both in TRANSITION.

The column list is the soundness surface. §Soundness explains how a missing column is ruled out.

### Outputs

| part | `PartOutput`, on the segment that holds it |
| --- | --- |
| TRACE | `refuted_trace_commitment`, `window_is_terminal`, `final_committed_root`, and the verdict: every step before the last matched, the last diverged |
| TRANSITION | final `EntrypointAuthorization` and `OutputAuthorization`; `replay_claims`: a chain over every tile step's `(registry image id, sha256(journal))`; `authorization`: `sha256(authorization journal)` |
| CONTROL FLOW, STORAGE | none: their whole contribution is their state, columns and record chain |

### Tile replays leave the critical path

TRANSITION reads each tile step's replay journal **as a value** and runs io, recur and drafts on it
exactly as today. It looks up the tile's image id in the program's registry, as today, and folds
`(image id, sha256(journal))` into `replay_claims`. It doesn't call `env::verify` on any replay.

FRAUD WINDOW verifies one receipt per tile step for the claims it is given, recomputes the
`replay_claims` chain from them, and compares it with TRANSITION's. By collision resistance, the
journal TRANSITION checked is the one the registered binary produced: the same guarantee as
today's `env::verify` inside io, split between two guests and tied by the digest. The authorization
journal works the same way.

So **no part depends on a proof.** TRANSITION needs replay journals, which the host gets by
*executing* tiles. Tile receipts can be proven per tile, in any order, on any machine, even while
the program runs, and they meet the rest only in FRAUD WINDOW.

To confirm in a prototype: whether a risc0 3.0 guest can verify a receipt from its claim digest
(`env::verify_integrity`) without the journal bytes. That keeps large tile outputs out of FRAUD
WINDOW. If it can't, FRAUD WINDOW reads the journal bytes, which is correct but heavier.

### FRAUD WINDOW

```rust
fn main() {
    let p = FraudWindowParams::read();     // InitTransition, program commitment
    // Part and authorization ids are compiled in (§Image-id pinning):
    // TRACE_ID, CONTROL_FLOW_ID, STORAGE_ID, TRANSITION_ID, AUTHORIZATION_ID.
    let segments: Vec<PartSegmentJournal> = frame::read();
    let tiles: Vec<(ImageId, Hash32)> = frame::read();     // replay claims, in step order
    let authorization: AuthorizationJournal = frame::read();

    for s in &segments {
        env::verify(part_image_id(s.part), &s.encode());     // a compiled-in constant
        assert_eq!(s.window_id, sha256(&p.init));
        if let Some(pc) = s.program_commitment { assert_eq!(pc, p.program_commitment); }
    }
    let parts = stitch(&segments, &p);       // per part: contiguous, covers the window,
                                             // records/state/columns chained end → start
    assert_all_equal(parts.final_record_chains());
    for column in Column::ALL {
        assert_eq!(parts.produced(column), parts.consumed(column));
    }
    for (image_id, journal_digest) in &tiles {
        env::verify_integrity(claim(image_id, journal_digest)); // see the prototype note
    }
    assert_eq!(chain(&tiles), parts.transition().replay_claims);
    verify_authorization_journal(&authorization, &AUTHORIZATION_ID);
    assert_eq!(sha256(&authorization), parts.transition().authorization);

    let trace = parts.trace();
    assert!(trace.verdict_diverges_at_last);
    env::commit(&FraudWindowJournal { /* §The final journal */ });
}
```

It runs no checks of its own. Its cost grows with the number of segments plus the number of tile
steps, not with what the steps do.

### Modes

Each part has a segment length (`window`, `<n>`, `1`), as control flow and storage do now:
`--trace-segment`, `--control-flow-segment`, `--storage-segment`, `--transition-segment`.
- **Per tile** (`1`): maximum parallelism, and TRANSITION or STORAGE can follow a run step by
  step.
- **Window-wise** (`window`): minimum fixed cost.
- **Mixed:** for example, storage per 8 steps and everything else window-wise.

Tile replays are always per tile and independent. The proof a relying party verifies is the same
in every mode.

### Image-id pinning

**Compiled in** (decided 2026-10-22). The build proves the four part guests and the authorization
guest first, writes their image ids into a generated source file, and builds FRAUD WINDOW with
them as constants. FRAUD WINDOW reads no image id as input, and its journal carries none but its
own implicitly: a relying party pins **one** id, `FRAUD_WINDOW_GUEST_ID`.

Consequences:
- **One pin.** `verify_fraud_proof_receipt` checks the receipt against `FRAUD_WINDOW_GUEST_ID`
  and nothing else about guests. The bug class of
  [`authorization-image-id-unpinned`](../issues/authorization-image-id-unpinned.md) (an id read
  as input and never pinned) can't recur for any part, because no part id is an input.
- **FRAUD WINDOW's identity covers the exact set of parts.** It can't be combined with a different
  build of any part.
- **Any part change moves FRAUD WINDOW's id.** Today a storage change moves the storage id, which
  relying parties also pin, so re-pinning doesn't increase; it goes through one id.
- **Two-pass build.** `risc0_build::embed_methods` builds all guests in one pass today. Likely
  shape: a method crate for the parts and authorization, and a second one for FRAUD WINDOW that
  takes the first's ids as a build input. Needs a prototype (§Migration step 0c). Builds get
  ordered: a part change rebuilds FRAUD WINDOW too.
- **Reproducible guest builds matter more.** A guest id depends on its ELF bytes, and ELFs can
  embed absolute source paths (seen with tile ELFs). Prover and relying party must build
  identically, which risc0's Docker builds provide. That's already true for every guest id; now
  FRAUD WINDOW's id depends on five builds.
- **Testing.** Stitching, comparison and claim checks live in a library function that takes the
  ids as a parameter, so they're unit-tested with any ids. `main` passes the constants. A "wrong
  part guest" end-to-end test needs a real receipt from another guest.
- **Unchanged:** tile image ids still come from the program's registry under `program_commitment`.

### The final journal

`FraudWindowJournal` (decided 2026-10-22), replacing `TransitionJournal`, which no guest commits
any more:

```rust
pub struct FraudWindowJournal {
    pub init_state: InitTransition,
    pub program_commitment: Vec<u8>,
    pub input_manifest_commitment: Vec<u8>,
    pub refuted_trace_commitment: Vec<u8>,
    pub window_is_terminal: bool,
    pub final_committed_root: Option<Vec<u8>>,
    pub entrypoint_authorization: EntrypointAuthorization,
    pub output_authorization: OutputAuthorization,
}
```

There's no `current_state`: a FRAUD WINDOW receipt exists only for a `Finished` window, so the
receipt itself is the verdict. There are no image ids, since they're compiled in. The fields a
relying party or a chain consumer reads are the same ones `TransitionJournal` carries, so those
consumers change their type, not their logic. `chain_fraud` doesn't verify fraud-proof receipts
and is unaffected.

### Changes to the existing guests

- **CONTROL FLOW and STORAGE** commit a `PartSegmentJournal` instead of an entries root. The
  per-step `ControlFlowEntry` / `StorageStepEntry`, their trees, paths and stdout entries, and
  `VerifiedJournals` all go away. STORAGE produces the two columns.
- **TRACE** is new, built from code moved out of `fraud_proof.rs`.
- **TRANSITION** is today's transition guest minus the trace, the window binding, the evidence
  checks, the `Next` chain (`proceed`, `verify_previous_journal`, the continuity asserts) and every
  `env::verify`. It gains the column chains, `replay_claims` and the record chain.
- **The host** executes every guest first to get journals and replay values, then proves all
  segments and tile replays at once with unresolved assumptions, and FRAUD WINDOW last, resolving.
  Today's host proves in sequence (`step_transitions`).

## Repository layout

As built (step 5):

```
crates/raster-core/src/fraud_window.rs   # PartKind, Column, Chain, WindowChains, PartSegmentJournal,
                                         #   PartOutput, FraudWindowParams, FraudWindowJournal,
                                         #   check_window (the aggregator's logic)
crates/raster-prover-guests/             # first pass of the guest build: every guest but FRAUD WINDOW
crates/raster-prover/guests/
├── trace/                               # the trace part
├── control_flow/                        # the control-flow part
├── storage/                             # the storage part, produces the two columns
├── transition/                          # the TRANSITION part: io, recur, drafts, entrypoint
├── fraud_window/                        # FRAUD WINDOW, part ids compiled in (src/ids.rs)
├── authorization/, chain_fraud/         # unchanged
└── guest_common/
crates/raster-prover/
├── build.rs                             # second pass: part ids -> FRAUD WINDOW
└── src/
    ├── window.rs                        # plan the three leaf parts, one proof job per segment
    ├── fraud_window.rs                  # plan TRANSITION, prove FRAUD WINDOW, verify the proof
    ├── schedule.rs                      # run_jobs: the worker pool
    └── transition.rs                    # StepIo, frame writers
```

## Soundness

**Claim:** FRAUD WINDOW accepts exactly when today's transition guest, run over the same window,
would reach `Finished`.

Today's guest is the conjunction of every check over one shared state. Each part is the
conjunction of its own checks over its own state, with the other parts' values replaced by host
values. FRAUD WINDOW restores the conjunction:
- every part ran on the same records in the same order (equal record chains), from the same
  initial state (`window_id`, each opening segment's own check) and program
  (`program_commitment`),
- every part's segments continue each other (stitched `state`), so a part's state is the one its
  own checks produced, step after step,
- every value one part read from another equals the value the producer computed (column chains,
  by collision resistance),
- every replay journal and the authorization journal TRANSITION checked is one their guests
  produced (claims verified in FRAUD WINDOW, chained and compared).

Under those, every check sees the inputs it sees today.

**The hazard is a missing column:** a cross-part read not listed in §Shared values would read a
host value nothing checks. That was this proposal's main risk in 2026-10-15 and still is. Three
defences:
1. **Typed inputs.** A part's step input can hold another part's value only as
   `Consumed<Column, T>`, which only a `ColumnReader` can unwrap, and unwrapping folds the value
   into the chain. Review then checks types, not every call.
2. **One table.** `Column::producer()` and `Column::consumers()` are the single definition that
   parts commit by and FRAUD WINDOW compares by.
3. **Tests.** A tamper test per column (change one value at one step → FRAUD WINDOW refuses), per
   stitch (a gap, an overlap, a reordered segment), and a **differential test**: today's pipeline
   and the split one accept and reject the same fixtures, including every negative test in the
   transition guest's suite.

The two columns were found by reading today's code. The typed inputs make the compiler re-find
them: step 1 of §Migration introduces them inside the existing guests, before anything moves.

## Cost

**Estimated**, from the measured guests (dev mode, user cycles, after the trims):

| | today | split (estimate) |
| --- | --- | --- |
| `hello-tiles` 27 | CF 2.56M + storage 3.40M + transition 4.53M = 10.50M, **15 segments** | CF ≈2.4M, storage ≈3.3M, TRACE ≈1.2M, TRANSITION ≈1.8M, FRAUD WINDOW ≈0.5–1M: ≈9.5M, **≈15–17 segments** |
| `prompt-prepare` 796 | 8.50M, **11 segments** | ≈7.5M, **≈12–14 segments** |

- **One GPU: about the same, possibly slightly more.** The per-step evidence checks (about a fifth
  of today's transition guest) and the inner guests' entry trees go away, but there's one more
  guest, each part pays its own setup, and every receipt is at least one risc0 segment.
- **Several GPUs: the window takes as long as its largest proof**, about 4–6 segments (storage or
  CONTROL FLOW), plus FRAUD WINDOW, instead of all of them in sequence. And tile replays, never
  measured with real proving, run alongside instead of before.
- **Long windows:** every part scales out by segments, with no chain anywhere. Only FRAUD WINDOW
  grows, by one stitch per segment and one claim per tile step.

## Migration

0. **Prototype and measure.** *Done 2026-10-22 (§Step 0 results, §GPU baseline).*
   - (a) risc0 3.0: prove with an unresolved assumption, resolve it later; and verify a receipt
     from a claim digest inside a guest.
   - (b) One real GPU run of today's pipeline on both windows, tile replays included. This is the
     baseline the split is judged against, and it measures one assumption's resolve (§Deferred).
     *Done 2026-10-22 (§GPU baseline).*
   - (c) The two-pass guest build: a guest compiled with another guest's image id as a constant.
1. **Columns inside today's guests.** *Done 2026-10-22 (§Step 1 record).* Make the two columns explicit in the transition guest as
   `Consumed<…>` (still checked against the storage entry as today), and add the record chain.
   No guest is added. If a cross-read was missed, this is where it fails to compile. Worth
   keeping even if the split stops here.
2. **Extract TRACE.** *Done 2026-10-22 (§Step 2 record).* Move the trace, window binding and verdict into `guests/trace`. Until FRAUD
   WINDOW exists, the transition guest verifies the TRACE receipt as it does control flow and
   storage today.
3. **Parallel host scheduling** *(done 2026-10-22, §Step 3 record)* with unresolved assumptions, for today's guests. Host-only. It
   pays as soon as more than one prover is available.
4. **FRAUD WINDOW.** *(done 2026-10-22, §Step 4 record)* Change the CONTROL FLOW and STORAGE journals, strip the transition guest down
   to TRANSITION, add `guests/fraud_window` with compiled-in part ids and `FraudWindowJournal`,
   behind `prove --fraud-window` with today's pipeline as the default. Add the per-column and per-stitch tamper tests and the differential test.
5. **Measure** both pipelines in every mode, on CPU dev mode and on GPU. Switch the default, then
   delete the per-step evidence machinery, `Next`, `Transition` and the continuity asserts.
   *(done 2026-10-22, §Step 5 record)*

## Step 0 results (2026-10-22)

Prototype outside the repository: three tiny guests (`inner`, an `impostor` committing the same
journal under another image id, and an aggregator `outer`), proven with **real receipts** on CPU,
no dev mode, risc0 3.0.6.

| item | result |
| --- | --- |
| (a) aggregator proven against an **unresolved** assumption, in parallel with the inner proof, then resolved | works. The conditional receipt does **not** verify on its own; after `resolve` it verifies under the aggregator's id. Outer and inner proven side by side in 18.5 s; one resolve 6.9–7.1 s (CPU) |
| (a) verification **from the journal digest only** (`env::verify_integrity(&ReceiptClaim::ok(id, Pruned(digest)))`) | works, same result. This is also what `env::verify` does internally: it hashes the bytes and asserts the same claim, so FRAUD WINDOW never needs a tile's journal bytes |
| (c) **two-pass build**: the inner image id compiled into the aggregator | works. A second methods crate takes the first as a **build-dependency**, reads its `*_ID` constant in `build.rs`, and passes it as an env var to the guest build (`risc0_build` strips only `CARGO*` and `RUSTUP_TOOLCHAIN`); the guest parses it with a `const fn`. An impostor receipt with an identical journal is refused at resolve ("assumption receipt claim does not match head of assumptions list") |
| (b) GPU baseline | **done the same day** once the GPU was free: §GPU baseline below |

### GPU baseline (2026-10-22)

Real proving (no dev mode) on an RTX 3070, 8 GB, CUDA build of `cargo-raster`, today's pipeline
after `transition-guest-trim` trims 1–2. risc0's default 2^20-cycle segments ran out of GPU
memory (also at 2^19), so every window guest was proven with 2^18-cycle segments, through a
temporary `segment_limit_po2` knob (removed). Tile replays use the default. Wall-clock includes
the host's witness building and execution.

| | `hello-tiles` 27 (32 steps) | `prompt-prepare` 796 (16 steps) |
| --- | --- | --- |
| tile replays | 18 proofs, **5.2 s** in all (~12k cycles, one small segment each) | 12 proofs, 4.4 s |
| control flow | 10.6 s (16 segments) | 8.1 s (13) |
| storage | 15.0 s (22) | 13.3 s (20) |
| transition | 23.7 s (33) | 15.8 s (23) |
| **all proving / wall-clock** | **54.5 s / 77 s** | **41.5 s / 53 s** |
| receipt | composite, 26.7 MB | composite, 20.7 MB |
| per-step everything (`1`/`1`/`1`) | 179.8 s, 78 MB | 91.7 s, 44 MB |
| control flow and storage `window`, transition `1` | 397.7 s (transition 369 s), 413 MB | — |
| compress the default receipt to succinct (lift, join, resolve) | **51.7 s** → 272 KB | **39.2 s** → 273 KB |

The first proof of a process also paid about 125 s of one-time CUDA initialisation, excluded
above.

What the GPU shows that dev mode couldn't:
- **Proving time follows segments**, at about 0.68 s per 2^18 segment for every window guest. So
  the dev-mode segment counts earlier in this series were the right measure.
- **Tile replays are negligible** on these programs: under 10% of proving, at about 0.3 s each.
  Proving them in parallel or early buys little here. The window guests are the cost.
- **No recursion runs today.** Every proof uses risc0's default `Composite` receipt kind: the fraud
  proof is a bundle of every segment of every guest, with each inner receipt nested whole. No
  lift, join or resolve is ever paid, and the receipt grows with the work (21–27 MB).
- **Chains of composite receipts grow quadratically.** With a per-step transition chain, each
  step nests the previous step's whole receipt, which nests the inner guests' receipts again: 413
  MB and 369 s of transition proving, against 23.7 s whole-window. `transition-segments` removed
  the worst of this. The FRAUD WINDOW design has no chain at all.
- **A succinct receipt costs about as much again** as proving (52 s and 39 s), and is 100× smaller.
  If relying parties need succinct receipts, most of that work is lift and join over each guest's
  own segments. In the split design each part can compress its own segments in parallel, leaving
  only the resolves for the end.
- **Resolve cost, for §Deferred's batching question:** one whole compression, including 21
  resolves and 90 lifts, is 52 s. Batching tile claims is not worth it at window sizes up to 32.
- **An 8 GB GPU needs 2^18-cycle segments** for these guests. That is a real configuration
  option to add (there's no risc0 environment variable for it).

What it changes in the design:
- **The host can't use `prove()` for a conditional receipt.** In risc0 3.0.6, `prove()` accepts an
  `Unresolved` assumption only when it matches a keccak receipt it produced itself
  (`prover_impl.rs:140–150`), whatever the receipt kind. The host has to do by hand what
  `composite_to_succinct` does, minus the resolve: `ExecutorImpl::from_elf(..).run()` to a
  session, then `prove_segment`, `lift` and `join` on every segment. About twenty lines, all
  public API. Then `ProverServer::resolve` per assumption once the inner receipts exist.
- **Docker builds need the id passed explicitly.** `risc0_build`'s Docker path forwards only the
  variables in `DockerOptions`, so the part ids go there too if reproducible builds use Docker.
- **The resolve cost is the input for §Deferred's batching decision:** about 7 s per assumption on
  CPU. Its GPU cost is still to measure.

## Prerequisite: the zkVM segment size (2026-10-22)

Before step 1: `--zkvm-segment-po2 N` on `run --audit` and `chain fraud-prove` (N in 13..=22,
risc0's default 20). It's the size of risc0's own proving segments, 2^N cycles, and is unrelated to
the `--*-segment` options, which count window steps. It is applied to every executor environment
the fraud path builds: tiles (`Risc0Backend::with_segment_limit_po2`), authorization, control
flow, storage, transition and chain fraud (`ZkvmSegmentPo2::env_builder`). risc0 3.0.6 has no
environment variable for it. `ProveOptions { segments, zkvm_segment_po2 }` now replaces the bare
`SegmentLengths` in `step_transitions` and the CLI. Verified: a real GPU proof of `hello-tiles` 27
with `--zkvm-segment-po2 18` in 71 s wall-clock, accepted by `verify_fraud_proof`. Out-of-range
values are refused at parse time.

## Step 1 record (2026-10-22)

- **`raster-core::fraud_window`**: `PartKind`; `Column { StoreBefore, Selections }` with
  `producer()` (Storage) and `consumers()` (Transition), the single table; `Chain` (a running hash
  per `ChainKind`, seeded with `window_id(InitTransition)`); `WindowChains { records,
  store_before, selections }` with `fold_all`, which folds the producer side from per-step values.
  `StoreBefore` folds `storage_state_digest(StorageState)`, `Selections` the step's
  `selection_witnesses_commitment`.
- **The transition guest** (`checks/columns.rs`): `Consumed<T>`, constructible only by
  `consume_store_before` and `consume_selections`, which fold the value into its chain.
  `advance_recur_progress`, `verify_program_end` and `verify_step_record` take
  `&Consumed<StorageState>` / `&Selections` instead of raw roots and maps. Making the change
  compiled exactly the cross-reads `§Shared values` lists, and no others. `apply_verified_step`
  consumes both once per step, right after the storage entry is equated; the selection commitment
  is computed once per step and passed to the storage facts too. `append_to_trace` folds the record
  chain.
- **Protocol:** `Transition` (the `Next` state) carries `chains: WindowChains`, and
  `TransitionJournal` commits them. No other journal or witness changes.
- **Host:** `step_transitions` folds the producer chains from the storage guest's committed entries
  and requires them equal to the journal's (`assert_consumed_columns_were_produced`). Inside the
  guest each step already equates its entry, so a mismatch would be a definitional bug. This is the
  comparison FRAUD WINDOW will make in a guest.
- **What moved:** `Transition`, `TransitionJournal` and the transition guest's image id. Also,
  unexpectedly, **every tile image id and `program_commitment`** of both `hello-tiles` (now
  `7ef04a4f…`) and `prompt-prepare`, although no tile calls the new code. The new `greet` ELF
  gained core's sort panic string, which points at a codegen change in `raster-core` rather than
  new tile code; not proven. `examples/hello-tiles/Raster.lock` is regenerated in the working tree.
- **End to end** (dev mode): `hello-tiles` fraud proofs at 27, 47 and 58, and at 27 with
  `--transition-segment 8 --storage-segment 4` (chains across `Next` handovers, segment
  boundaries differing between the two guests); the host's producer/consumer check passed on each.
  An honest `hello-tiles` audit; `prompt-prepare` 796; `verify_fraud_proof` on the segmented proof.
- **Tests:** `raster-core` (`fraud_window`, 3), the transition guest (160: two new column tests;
  the tests that drive a consumer directly wrap raw values with the test-only
  `Consumed::unchecked`), storage (18), control flow (55), `raster-prover` (54).

## Step 2 record (2026-10-22)

- **`guests/trace`** proves a segment of steps. It reads a `TraceSegmentHeader` (the
  `InitTransition`, the commitment header, the fingerprint slice witness, the revealed tail roots,
  the start frontier and the step count), then one record hash per step. It runs the window binding
  (`assert_window_is_commitment_slice` and `assert_opens_at_genesis`, moved unchanged from the
  transition guest with their 16 tests) and appends each hash. It applies the verdict per step: the
  root's packed bits equal the committed item, or, at the window's last index, differ from it or
  from the revealed root. It commits `TraceJournal { window_id, refuted_trace_commitment,
  window_is_terminal, final_committed_root, first_position, entry_count, entries_root }` over
  `TraceStepEntry { step_record_hash, trace_position, trace_root_before, trace_root_after }`.
  Every segment re-binds the window, so a segment needs nothing from another.
- **The verdict needs no carried state.** At window index `i` it compares
  `fingerprint_value(root_i)` with committed item `i`; today's accumulator only ever fed that one
  comparison. So the fingerprint accumulator left the protocol: `Transition` now carries only
  `trace_position` and `trace_root`.
- **The transition guest** verifies the TRACE receipt per step (`checks/trace.rs`, receipt once per
  execution): the entry's path, its record hash, its position, and that its `trace_root_before`
  is the root the previous step ended on. That chaining, by position and root, binds the frontier
  every segment appended to, so a segment the host started elsewhere is refused. It also requires
  the journal to name this window (`window_id`) and the binding taken from the window's first
  step. The root before goes to the control-flow check as before. It decides `Finished` by
  position, since every covered step was judged. It no longer reads the commitment header or slice
  witness, and `TransitionInput` no longer carries the revealed tail roots.
- **Pinning:** `trace_image_id` is read as a frame, held continuous across `Next`, committed in
  `TransitionJournal`, and pinned by `verify_fraud_proof_receipt`.
- **Host:** `raster_prover::trace_segments` (`plan_trace_segments`, `prove_trace_segment`,
  `ProvenTrace`, `trace_guest_image_id`); `SegmentLengths::trace` and `--trace-segment`.
- **Tests:** trace guest 23 (16 moved, 7 new on the append and verdict: one and two segments
  commit identical entries, no divergence refused, an early divergence refused, an opening segment
  from another frontier refused, a segment past the window refused). Transition guest 155 (16
  moved out, the tail-roots input test dropped, 10 on the TRACE consumer, 2 continuity).
  `raster-core` 208 (2 `trace_check`), `raster-prover` 55 (the TRACE pin; the in-zkVM single-step
  test proves a real TRACE receipt, which now carries that step's verdict).
- **End to end** (dev mode): `hello-tiles` fraud proofs at 27, 47 and 58; at 27 with every
  segment guest on a different length (`--trace-segment 5 --storage-segment 4
  --control-flow-segment 3 --transition-segment 8`), and with TRACE and transition per step; an
  honest audit; `prompt-prepare` 796. `verify_fraud_proof` accepts the mixed-mode proof. Both
  programs' locks were unchanged this time.
- **Measured on the GPU** (RTX 3070, `--zkvm-segment-po2 18`, default modes; baseline is §GPU
  baseline):

  | | transition | TRACE | all proving |
  | --- | --- | --- | --- |
  | `hello-tiles` 27, before | 23.7 s / 33 segments | — | 54.5 s / 89 |
  | `hello-tiles` 27, after | **20.0 s / 30** | 4.6 s / 7 | **53.1 s / 93** |
  | `prompt-prepare` 796, before | 15.8 s / 23 | — | 41.5 s / 68 |
  | `prompt-prepare` 796, after | **14.2 s / 21** | 2.6 s / 4 | **43.0 s / 70** |

  **Cost-neutral on one GPU**, within run-to-run noise: control flow and storage alone varied by
  about 7% between the two runs. That's as §Cost estimated. The transition guest is smaller, and
  TRACE can be proven alongside it. The real GPU receipt verifies with all five guests pinned.

## Step 3 record (2026-10-22)

- **Order kept, assumptions not left unresolved.** A window's leaf proofs (tile replays,
  authorization, every trace, control-flow and storage segment) are independent and now run as one
  batch; the transition chain runs after them with their receipts as ordinary assumptions. The
  unresolved-assumption path (step 0) is not used: it needs the lift/join/resolve recursion,
  which §GPU baseline measured at about as long as proving itself, while today's composite
  receipts skip it. It remains the way to overlap the transition guest too, once succinct
  receipts are required anyway.
- **`raster_prover::schedule`**: `run_jobs(workers, jobs)` runs boxed jobs on up to `workers`
  scoped threads and returns results in job order; with one worker it runs them in order on the
  calling thread, as before. `ProverWorkers` (default 1) is `ProveOptions::workers`, set with
  `--parallel-provers` on `run --audit` and `chain fraud-prove`.
- **`raster_prover::window`**: `WindowSetup` (everything a window's parts are planned from,
  including `init_transition()`), `plan_parts`, `part_jobs` (one job per planned segment) and
  `assemble_parts`; `ProvenControlFlow`, `ProvenStorage` and `ProvenTrace` gained
  `from_segments`. `step_transitions(setup, parts, …)` now proves only the transition chain.
- **CLI:** the witness loop collects tile replays instead of proving them inline. The program frame
  is built first, which compiles every tile once, so no two workers build a tile at the same time.
  Then one `run_jobs` call proves every leaf.
- **Where more workers pay:** risc0's in-process prover puts every proof on the default GPU, so
  on a GPU that holds one proof at a time (the 8 GB RTX 3070 at 2^18-cycle segments) keep one
  worker. Several workers pay on CPU, on a GPU with memory for several proofs, or with a remote
  prover. risc0's separate `r0vm` prover (`RISC0_DEFAULT_PROVER_NUM_GPUS`) spreads one proof's
  segments across GPUs on its own, which is complementary.
- **Verified.** `raster-prover` 59 (4 new for the pool). Dev mode, end to end with
  `--parallel-provers 1` and `4`: `hello-tiles` 27, 47 and 58, 27 with mixed segment lengths and 4
  workers, an honest audit, and `prompt-prepare` 796. **The fraud proofs from 1 and 4 workers are
  byte-identical** on both programs, and verify with all five guests pinned. Dev mode still
  executes every guest, so 4 workers also cut those audits from about 46 s to 33 s
  (`hello-tiles`) and 38 s to 29 s (`prompt-prepare`).
- **Real GPU** (RTX 3070, `hello-tiles` 27, wall-clock):

  | workers | zkVM segment | wall-clock |
  | --- | --- | --- |
  | 1 | 2^18 | 75 s (unchanged) |
  | 1 | 2^17 | 99 s |
  | 2 | 2^17 | 93 s |

  Two workers fit this card only at 2^17-cycle segments, and gain 6% there, less than the
  smaller segments cost. So on one 8 GB GPU the best setting stays 1 worker at 2^18. The 2-worker
  receipt verifies too.

## Step 4 record (2026-10-22)

Both pipelines are built and run side by side; today's stays the default, and `--fraud-window`
(on `run --audit`) selects the new one.

- **Two-pass guest build.** A new `crates/raster-prover-guests` crate builds every guest but FRAUD
  WINDOW (`embed_methods`). `raster-prover` takes it as a build-dependency, sets the trace,
  control-flow, storage, TRANSITION and authorization ids as `RASTER_PART_ID_*` environment
  variables in its `build.rs`, and builds `guests/fraud_window`, which compiles them in with
  `env!` (`src/ids.rs`). `risc0_build` passes the build script's environment to the guest build,
  stripping only `CARGO*` and `RUSTUP_TOOLCHAIN`; a Docker build would have to forward them
  explicitly. `raster-prover` re-exports the first pass, so every existing `crate::*_GUEST_ID`
  path is unchanged.
- **One journal for every segment.** `ControlFlowJournal`, `StorageJournal` and `TraceJournal`
  are gone; all four parts commit `PartSegmentJournal` (`raster-core::fraud_window`): part,
  `window_id`, program commitment (for the parts that read it), extent, record chain, state
  digest and column chains before and after, and the part's output. While today's pipeline lives,
  it also carries `entries_root`, which today's transition guest checks per step and FRAUD WINDOW
  ignores; it goes in step 5. The control-flow and storage segment headers carry
  `InitTransition` and the chains before; each part's opening segment checks its start against
  `InitTransition` itself (control flow: initial frontier, nothing expected; storage: initial
  store, and its root is the frontier's).
- **TRANSITION is a second binary of the transition guest crate.** The checks moved to a library
  (`src/lib.rs`); `src/main.rs` is today's guest, `src/bin/transition_part.rs` the TRANSITION
  part (`part::verify_segment`), so both run the same semantic checks and the transition suite
  covers both. The part verifies no receipt: it reads the store before each step and the
  selection witnesses through `Consumed` as before, now from the host; folds each tile step's
  `ReplayClaim` (registry image id, `sha256` of the replay journal) into a chain instead of calling
  `env::verify` (io returns the claim; today's guest verifies it with `env::verify_integrity`);
  and commits the authorization journal as a digest. Its state (recur progress, entrypoint and
  output authorization) is committed as a digest and written to stdout for the next segment.
- **FRAUD WINDOW** (`guests/fraud_window`) verifies each segment journal against its part's
  compiled-in id, each replay claim with `env::verify_integrity` on its claim digest (the step 0a
  prototype), and the authorization journal; then `fraud_window::check_window`, a pure function in
  `raster-core`, stitches and compares, and the guest commits `FraudWindowJournal`.
- **Host** (`raster_prover::fraud_window`). Tiles are *executed* first
  (`Replayer::execute`, a journal-returning estimate run; `TileExecutionResult::journal`), and the
  authorization guest too (`execute_authorization`). `plan_transition_part` executes each
  TRANSITION segment but the last, in order, for the next one's start state. Then one
  `run_jobs` batch proves the tile replays, the authorization and every segment of all four parts,
  and `prove_fraud_window` proves the aggregator with every receipt as an assumption.
  `verify_fraud_window_receipt` pins `FRAUD_WINDOW_GUEST_ID` alone. The chain path
  (`chain fraud-prove`) stays on today's pipeline: `chain_fraud` reads `TransitionJournal`.
- **Tests.** `check_window`: 14 (an honest window in any order and mixed segment lengths; a
  tampered value in every column; different records; a gap, an overlap, a repeat, a short or
  overlong part, a missing part; every kind of broken stitch; swapped segments; chains not from
  the seed; another window or program; missing columns or output; other replay claims; another
  authorization journal). Guests: trace 24, control flow 58, storage 22 and transition 162, the
  new ones for the journals, opening checks and stitching. Host: the differential test proves one
  window through both pipelines, checks the two journals agree on every fact a relying party
  reads, and that FRAUD WINDOW refuses when any one of its assumptions is missing.
- **End to end, dev mode:** `hello-tiles` 27, 47 and 58 with `--fraud-window`, 27 with mixed
  segment lengths (trace 5, storage 4, control flow 3, transition 8) and 4 workers, 47 with every
  part per step, an honest audit, and `prompt-prepare` 796. Every proof verifies against
  `FRAUD_WINDOW_GUEST_ID` alone and names the refuted `commit.bin`. Today's pipeline, still the
  default, proves and verifies as before. The tile locks didn't move.
- **Real GPU** (RTX 3070, 2^18-cycle segments, one worker, wall-clock for the whole audit):

  | window | today's pipeline | FRAUD WINDOW |
  | --- | --- | --- |
  | `hello-tiles` 27 | 84 s, 82 s | 69 s |
  | `prompt-prepare` 796 | 57 s | 51 s |

  Both receipts are composite and verify; FRAUD WINDOW's is 24 MB against 31 MB. On one GPU the
  split is already 10–18% faster, against §Cost's estimate of "about the same": TRANSITION drops
  the per-step evidence checks and every `env::verify`, and nothing waits on a `Next` chain.
  Today's pipeline got slower than in step 3 (75 s on `hello-tiles`): its part guests now also
  fold the chains and hash `InitTransition`. Step 5 measures every mode properly.
- **Left for step 5:** delete today's pipeline (the per-step entries, their trees and evidence,
  `entries_root`, `VerifiedJournals`, the `Next` chain and `TransitionJournal`), make
  `--fraud-window` the only path, move `chain fraud-prove` onto `FraudWindowJournal`, and rewrite
  the interface docs that still describe per-step evidence (`raster-core::trace_check` and
  friends). Step 4 keeps both pipelines so step 5 can measure them against each other.

## Step 5 record (2026-10-22)

FRAUD WINDOW is the only pipeline. `--fraud-window` is gone; `run --audit` and
`chain fraud-prove` always prove through it.

- **Deleted:** the transition chain (`step_transitions`, the `Next` journal, `Transition`,
  `TransitionState`, `TransitionInput`, `TransitionSegmentHeader`, `TransitionJournal`), the
  continuity asserts, `verify_fraud_proof_receipt` and `check_fraud_proof_journal`; the per-step
  evidence (`ControlFlowEvidence`, `StorageEvidence`, `TraceEvidence`), the entry trees, paths and
  leaf hashes, and `raster-core::segment_tree`; `PartSegmentJournal::entries_root`; the part
  guests' entries on stdout; `VerifiedJournals` and the transition guest's evidence checks
  (`checks::{trace, control_flow, store, receipts}`, `fraud_proof.rs`).
- **The transition guest is the TRANSITION part.** `src/bin/transition_part.rs` became
  `src/main.rs`, so its image id is `TRANSITION_GUEST_ID` again. The library keeps the semantic
  checks and `part::verify_segment`; the two helpers it used from `fraud_proof.rs`
  (`expected_tile_image_id`, `assert_seed_present_for_mid_loop_open`) moved into `part.rs`.
- **One proven-segment type.** All four parts return `window::ProvenPartSegment { receipt,
  journal }`. The host no longer reassembles segments in order: FRAUD WINDOW takes them in any
  order and `check_window`'s coverage check refuses a window any of them is missing from.
  `FraudWindowInputs` takes one slice of segments.
- **`FraudWindowJournal` took over `TransitionJournal`'s field docs** (why `window_is_terminal`
  matters, what `final_committed_root` makes provable). `availability` and the
  `authorization-image-id-unpinned` issue now describe the one-id pin.
- **`chain fraud-prove`** verifies its terminal-window receipt with
  `verify_fraud_window_receipt`. Testing it end to end found that this path has been unprovable
  since `c484ef7` (2026-09-16), before this work: its window's last step matches its own honest
  commitment, and the trace guest requires the last step to diverge. Filed as
  [`terminal-window-refused`](../issues/terminal-window-refused.md); the fix is a design choice
  between a verdict kind in the journal and a separate aggregator id.
- **Tests.** raster-core 212 (one new: TRANSITION segments that read different authorization
  journals don't stitch), trace 24, control flow 58, storage 22, transition 105 (one new: a later
  segment may not carry an entrypoint witness; the evidence and continuity tests went with their
  code), raster-prover 50. The host tests now prove a window through FRAUD WINDOW, refuse it
  with any one of its five assumptions missing, and refuse a tampered authorization journal in
  the guest; the step-input tests were ported to `fraud_window::transition_step`. `raster-cli`: 122
  pass (83 unit, 39 integration across four suites), 1 ignored.
- **End to end, dev mode:** `hello-tiles` 27, 47 and 58, 27 with mixed segment lengths and 4
  workers, 47 with every part per step, an honest audit, and `prompt-prepare` 796: every proof
  verifies against `FRAUD_WINDOW_GUEST_ID` and names the refuted `commit.bin`. The tile locks of
  `hello-tiles`, `prompt-prepare` and the chain example's three stages didn't move.
- **Measurement.** The plan was to measure both pipelines in every mode before deleting one. That
  failed: an audit compiles the program against the live workspace, so deleting the legacy guest
  while the matrix ran broke every run after the first, and the deleted step-4 sources weren't
  recoverable from git. What exists for the legacy pipeline is §GPU baseline (window and
  all-per-step) and §Step 4 record (window). FRAUD WINDOW in every mode is measured below.

### GPU, step 5 (RTX 3070, 2^18-cycle segments)

Wall-clock for the whole audit (execution, tile replays, every part, FRAUD WINDOW), every part at
the same segment length, one worker unless noted. Every receipt is composite and verifies.

| mode | `hello-tiles` 27 (32 steps) | `prompt-prepare` 796 (16 steps) |
| --- | --- | --- |
| `window` | **71 s**, 23.1 MB | **53 s**, 19.5 MB |
| `4` | 94 s, 35.6 MB | 60 s, 24.9 MB |
| `1` (per step) | 180 s, 82.9 MB | 101 s, 47.1 MB |
| `1`, 2 workers at 2^17 | 198 s, 170 MB | — |

Against the legacy pipeline, from §GPU baseline and §Step 4 record:

| | `hello-tiles` 27 | `prompt-prepare` 796 |
| --- | --- | --- |
| legacy, window (step 0 / step 4) | 77 s / 84 s, 26.7 MB | 53 s / 57 s, 20.7 MB |
| legacy, all per step (step 0, proving only) | 179.8 s, 78 MB | 91.7 s, 44 MB |
| FRAUD WINDOW, window | 71 s, 23.1 MB | 53 s, 19.5 MB |
| FRAUD WINDOW, all per step (wall-clock) | 180 s, 82.9 MB | 101 s, 47.1 MB |

What it shows:
- **Window mode stays the default.** It is the fastest mode on one GPU for both programs, and
  FRAUD WINDOW's window mode is as fast as the fastest legacy measurement or faster, with a
  smaller receipt.
- **Shorter segments cost on one GPU,** about 1.3× at `4` and 2–2.5× per step, because every
  segment pays its guest's fixed work and at least one zkVM segment. The legacy per-step chain
  grew quadratically when only the transition guest went per step (§GPU baseline: 413 MB);
  FRAUD WINDOW per step grows linearly, since nothing nests a previous receipt.
- **Two workers didn't pay on this card**: 2^17-cycle segments, which two concurrent provers need
  in 8 GB, cost more than the overlap gains, and double the receipt size (170 MB). Per-step mode
  is for several provers with their own memory, as §Step 3 record found.

## Decisions (2026-10-22)

1. **The final journal is `FraudWindowJournal`** (§The final journal).
2. **Part and authorization ids are compiled into FRAUD WINDOW** (§Image-id pinning). One pin for
   relying parties, at the cost of a two-pass build.
3. **TRACE stays separate from CONTROL FLOW.** Both fold the same trace tree from the same records,
   but control flow keeps no knowledge of `commit.bin`, and each answers one question.

## Deferred

- **Batching tile claims.** Every tile step's replay receipt is one assumption of FRAUD WINDOW's
  receipt, and each is resolved in recursion when FRAUD WINDOW is proven. That's not new: today's
  transition guest resolves the same one per tile step (18 on the `hello-tiles` window, 12 on
  `prompt-prepare`). It matters for large windows: `--fraud-proof-window-size` allows up to 1,024
  steps, and FRAUD WINDOW would resolve all of them at the end, in sequence. A batching guest
  would verify, say, 32 claims and commit one digest. Total resolve work stays the same but moves
  earlier and spreads out, as batches resolve in parallel and as soon as their tiles are proven.
  **Measured 2026-10-22:** compressing a whole 32-step window's receipt, 21 resolves and 90 lifts
  included, takes 52 s on an RTX 3070, about as long as proving it. Not worth batching up to 32
  steps. Revisit only for much larger windows.

## History

**2026-10-15: seven parts by check family.** `trace`, `control_flow`, `recur`, `io`, `store`,
`drafts`, `entrypoint`, plus an aggregator equating "column" digests, with every part over the
whole window and `Next` removed. Two things changed the plan:
- **Two families were extracted another way.** Control flow and storage became segment guests whose
  per-step entries the transition guest checks (`control-flow-prover`, `storage-prover`), and
  `transition-segments` removed the per-step chain without an aggregator.
- **Measurement ruled out the small parts.** io, recur, drafts and entrypoint together are 11–16%
  of the transition guest, while consuming a separate guest cost 20–37k cycles a step, more than
  io (15–25k) or recur (4–14k). As separate guests they'd be mostly overhead
  (`transition-guest-trim`). They also share their main input, the replay journal.

What carried over: the column idea (now chains instead of whole-column digests, so segments of any
length can be stitched), the typed-input and differential-test defences, the pinning options, and
the aggregator itself, now FRAUD WINDOW. The seven-part version is in git history (`e3a37fe`).
