# Proposal: `transition-guest-trim` — don't split the transition guest further; trim what it repeats

Status: **Trims 1 and 2 implemented 2026-10-22** (§Implementation record), measured in dev mode:
the transition guest is 23% and 20% smaller. Trim 3 is not done (open question 1). io stays in the
transition guest (decided 2026-10-22). The cost split is **measured** (§Where the transition
guest's cycles go).

The question this answers: after [`control-flow-prover`](./control-flow-prover.md),
[`storage-prover`](./storage-prover.md), [`transition-segments`](./transition-segments.md) and
[`compact-coordinate-index`](./compact-coordinate-index.md), the transition guest is the largest of
a window's three receipts. Should the next step be extracting another check family, io or recur,
into its own guest? **No.** Every remaining family is smaller than what the transition guest pays
per step to consume an extracted guest's receipt. What makes the guest big is work it repeats:
recomputing the trace root, hashing the same record and arguments three times, and decoding
evidence fields it already holds.

Related:
- [`transition-split`](./transition-split.md): asked the same question for every family (its step
  0). This measurement answers it for the families that are left.
- [`transition-segments`](./transition-segments.md): the whole-window execution measured here.
- [`storage-prover`](./storage-prover.md) fix 1: the same "cache the root" trim, already applied to
  the storage guest's append log.

## Where the transition guest's cycles go

Temporary `env::cycle_count()` probes inside the transition guest, not committed. Default modes
(one execution per window), dev mode, `hello-tiles --fraud-step 27` (32 steps) and
`prompt-prepare --fraud-step 796` (16 steps). The probes add about 5%: 6.18M and 4.11M against
5.86M and 3.95M uninstrumented.

| | `hello-tiles` | `prompt-prepare` |
| --- | --- | --- |
| whole guest | 6.18M | 4.11M |
| **trace frontier**: root before each step (27.6k a step) + append and root after (33–34k a step) | **1.94M (31%)** | **0.99M (24%)** |
| **input frame decode** (one `TransitionInput` a step) | **1.34M (22%)** | **0.93M (23%)** |
| **storage evidence**: receipt (once), entry hash and path, record hash, input-source hash, selection-map hash, equalities | **0.82M (13%)** | **0.59M (14%)** |
| **control-flow evidence**: receipt (once), entry hash and path, record hash, input-source hash, equalities | **0.63M (10%)** | **0.38M (9%)** |
| io: replay encode and `env::verify` 0.17 / 0.12, input-source anchor 0.12 / 0.11, input roots 0.05 / 0.11, the rest 0.13 / 0.07 | 0.47M (8%) | 0.40M (10%) |
| recur: progress and chunking | 0.13M (2%) | 0.23M (5%) |
| drafts | 0.06M | 0.03M |
| entrypoint (only `ProgramStart`/`ProgramEnd` do work) | <0.01M | <0.01M |
| once per execution: program decode and `CfsCursor` 0.17 / 0.16, window binding and genesis 0.16 / 0.15, `Init` binding decode 0.04, authorization 0.01, commit 0.01 | 0.39M (6%) | 0.37M (9%) |
| per-step asserts, verdict, drops | 0.14M | 0.06M |

### Why not extract io or recur

Consuming an extracted guest costs the transition guest, every step: verifying the step's entry
against the root (the path fold and the leaf hash), the equalities, and decoding the evidence.
The control-flow and storage rows show that cost at **19.5k and 25.7k a step** (`hello-tiles`) and
**23.5k and 36.7k** (`prompt-prepare`), even after the receipt itself is verified once per
execution.

io does **14.7k and 25.1k a step**, and recur **4.1k and 14.2k**. Extracting either would add more
consumption cost than it removes, before counting the new guest's own decode, fixed work and at
least one more risc0 segment. The result is the same with parallel proving: the transition guest
(8 and 5 segments) is already close to storage (5 and 4) and control flow (4 and 3), and moving
io out could save it at most about one segment while adding a receipt.

Together, io, recur, drafts and entrypoint are **11% and 16%** of the guest. The rest is the trace
frontier, input decoding, and consuming the two guests already extracted. That is where to trim.

## Design: three trims, no new guest

### Trim 1: cache the trace root

`apply_verified_step` folds the trace frontier to its root before the step (`trace_root_before`,
for the control-flow entry), and `append_to_trace` folds it again after the append, for the
fingerprint. The root after step `k` *is* the root before step `k + 1`. `LiveTransition` keeps the
last root, as `StorageCursor` keeps the append-log root since `storage-prover` fix 1, and only the
fold after each append remains.

Change: one field in `LiveTransition`, set at genesis and resume, and updated in
`append_to_trace`. No protocol change. **Estimate: −0.88M and −0.44M (14% and 11%).**

### Trim 2: hash the record and the arguments once per step

Three checks hash the same values:
- `hash_trace_item(record)`: in the control-flow evidence, the storage evidence and
  `append_to_trace`. About 4–5k cycles each.
- `input_source_commitment(witness)`: in the control-flow evidence, the storage evidence and io's
  anchor. About 3.7k and 6.7k each.

`apply_verified_step` computes both once and passes them through `ControlFlowFacts`,
`StorageFacts`, `verify_input_source_anchored` and the trace append, which take hashes instead of
the values. No protocol change. **Estimate: −0.5M and −0.4M (8–10%).**

### Trim 3: rebuild evidence entries instead of decoding and equating them

Each step decodes a whole `ControlFlowEntry` and `StorageStepEntry`, then asserts field by field
that each equals the transition guest's own value: record hash, input-source commitment, trace
position, trace root, expected coordinates, and the storage state before. Every one of those is a
value the transition guest already holds. Only the guest's outputs are new to it: the control-flow
entry's `expected_after`, and the storage entry's `after` state.

So the evidence carries only those outputs and the path:

```rust
pub struct ControlFlowEvidence { pub journal: u32 /* index into the execution's journals */,
                                 pub expected_after: Vec<CfsCoordinates>, pub path: Vec<Hash32> }
pub struct StorageEvidence     { pub journal: u32, pub after: StorageState, pub path: Vec<Hash32> }
```

The transition guest builds the entry from its own facts plus the output, hashes the leaf, and
checks that it folds to the journal's root at the step's index. The journals themselves are sent
once per execution, not once per step.

**Soundness is unchanged.** Today's check is "the committed entry equals my facts, field by field,
and is committed at my index". The new check is "the entry built from my facts is committed at my
index". By collision resistance of the leaf hash these accept the same steps. A wrong fact now
shows up as "entry not committed" rather than naming the field. To keep debugging easy, the host
can rebuild the entry from the guest's journal and diff it before proving, and report the field.

Change: `ControlFlowEvidence`, `StorageEvidence` and the journals' framing in `TransitionInput`,
so the host and the transition guest change. The segment guests and their journals don't. That
removes per step two entries' decode (about ten fields) and the equalities. **Estimate:
−0.3 to −0.6M.** Unmeasured, because the decode probe doesn't separate the evidence from the rest
of the input.

### Together

**Estimate: 6.2M → about 4.3–4.7M user cycles (`hello-tiles`), 4.1M → about 3.0–3.3M
(`prompt-prepare`).** That is roughly 8 → 6 and 6 → 4–5 segments, out of windows of 17 and 13 (as
measured with the probes in). That is a modest gain: about 12% of a window. The levers that remain
for a window's proving time are outside this guest: proving the three receipts in parallel on more
than one GPU, and confirming everything under real (GPU) proving.

## Migration

1. **Trim 1.** `LiveTransition` caches the trace root. Transition-guest tests pass, and `hello-tiles`
   and `prompt-prepare` still generate fraud proofs. Measure.
2. **Trim 2.** Hashes computed once in `apply_verified_step` and passed down. `ControlFlowFacts`
   and `StorageFacts` take hashes. Measure.
3. **Trim 3.** Evidence carries outputs and a path; journals once per execution; the entry is
   rebuilt in the transition guest; the host pre-checks and reports field mismatches. Update the
   tamper tests: each changed fact must still be refused, now as "not committed". Measure.

Steps 1 and 2 change only the transition guest's code, and so its image id. Step 3 also changes
the host input format. None changes a commitment, a journal shape or a segment guest.

## Open questions

1. **Is trim 3 worth its protocol change?** Its gain is the least certain and the only one that
   touches the host. Leaning: do 1 and 2, measure, and do 3 only if decode is still the largest
   row.
2. **Should `transition-split` be closed?** *Settled 2026-10-22: rewritten instead.* It now splits by
   data source into four parts (TRACE, CONTROL FLOW, STORAGE, TRANSITION) and a FRAUD WINDOW
   aggregator. io, recur, drafts and entrypoint stay together as TRANSITION, as measured here.

## Implementation record (2026-10-22)

**Trim 1.** `LiveTransition.trace_root` is set at genesis and resume (one fold each) and updated
in `append_to_trace`. `apply_verified_step` reads it as the control-flow entry's
`trace_root_before`, and `reaches_window_end` compares it with the revealed tail root.

**Trim 2.** `apply_verified_step` computes `hash_trace_item(record)` and
`input_source_commitment(witness)` once per step. `ControlFlowFacts` and `StorageFacts` carry
`step_record_hash` and `input_source_commitment` instead of the record and the witness.
`append_to_trace` takes the hash. io's `verify_step_record` takes the commitment and anchors it
with the new `guest_common::input_source::verify_input_source_commitment_anchored`.
`verify_input_source_anchored` keeps its signature and its panic messages, and is now a wrapper
over it. The control-flow and storage guests still hash the witness themselves, once for the anchor
and once for their entry; that is a few thousand cycles per step in each and was left alone.

**Tests.** Unchanged in what they check. The control-flow and storage scenario tests build their
facts from the hashes, so tampering with the transition guest's witness (a renamed argument) is
still refused, now at the hash it produces. All suites pass: the transition (158), storage (18)
and control-flow (55) guests, and `raster-prover` (53). No protocol change: same journals,
witnesses and commitments. Only the transition guest's image id moves.

**Measured** (dev mode, risc0 `SessionStats`, default modes; before is
`compact-coordinate-index` §Measured):

| | transition guest before | after | window before | window after |
| --- | --- | --- | --- | --- |
| `hello-tiles` 27 | 5.86M / 7.60M / 8 | **4.53M / 6.29M / 6** (−23%) | 11.82M / 15.34M / 17 | **10.50M / 13.89M / 15** |
| `prompt-prepare` 796 | 3.95M / 5.24M / 5 | **3.17M / 4.19M / 4** (−20%) | 9.28M / 12.06M / 12 | **8.50M / 11.01M / 11** |

Inside the estimate of 19–24%. `hello-tiles` 47 and 58: transition guest 4.92M / 7 segments and
4.64M / 6, and `verify_fraud_proof` accepts the step-27 proof. Per-step transition mode on
`hello-tiles`: 13.23M / 30.93M / 32 segments. The transition guest is now about the size of the
storage guest (6 and 5 segments, 4 and 4).
