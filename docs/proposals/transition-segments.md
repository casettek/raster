# Proposal: `transition-segments` — prove a run of window steps in one transition-guest execution

Status: **Implemented 2026-10-21** (§Implementation record). Proposed the same day. Steps 1–3
landed together. Step 4 is measured in dev mode on `hello-tiles` and `prompt-prepare`
(§Measured after implementation): whole-window mode cuts the transition guest from one receipt
per step to one per window, from 33 risc0 segments to 8 and from 18 to 6, and the window as a
whole from 73 to 48 and from 43 to 31. `window` is the default for all three guests. GPU timing
is still to run.

Scope: **the transition guest itself.** Today it proves one window step per execution and chains
the executions through `TransitionState::Next` journals. This proposal lets one execution prove a
**segment** of consecutive steps. Its length is chosen by the host, from one step to the whole
window, as [`control-flow-prover`](./control-flow-prover.md) §Segments and
[`storage-prover`](./storage-prover.md) already do for their guests. The checks, the journal and
what a relying party pins stay the same.

Related:
- [`transition-split`](./transition-split.md): its aggregator also proves a window without a
  per-step chain. §Relation to `transition-split` explains why this is a different route to the
  same place, not a duplicate of it.
- [`control-flow-prover`](./control-flow-prover.md), [`storage-prover`](./storage-prover.md): the
  two segment guests already extracted. Their receipts are assumptions of every transition step,
  so how a transition segment consumes them is part of this design (§Inner receipts).
- [`chain-fraud-proof`](./chain-fraud-proof.md): consumes the `Finished` `TransitionJournal`,
  which keeps its shape.
- [`window-seed-reconstruction`](./window-seed-reconstruction.md): the recur seed, read once at a
  window's `Init`, which still happens exactly once.

## Problem

After the control-flow and storage guests were extracted, the transition guest is cheap per step.
But each step is still a separate execution, and those executions form a strict chain:

```
step k:  read program.bin, build CfsCursor ─► env::verify(prev TransitionJournal)
         ─► env::verify(authorization journal) ─► env::verify(control-flow journal)
         ─► env::verify(storage journal) ─► checks ─► finalize ─► commit TransitionJournal(Next)
step k+1: the same, starting by verifying step k's journal
```

Measured on the same windows as `storage-prover` §Measured after implementation (dev mode):

| | `hello-tiles`, 32 steps | `prompt-prepare`, 16 steps |
| --- | --- | --- |
| transition guest, user cycles | 13.90M (434k / step) | 8.79M (549k / step) |
| transition guest, total cycles | 31.20M (975k / step) | 17.30M (1.08M / step) |
| transition guest, segments | **33** | **17** |
| storage + control-flow guests, segments | 40 | 25 |

Three things follow:

1. **The chain is at its floor.** Every execution is at least one risc0 segment, and the chain is
   already one segment per step (plus one). Extracting another check family (io, recur) makes
   each step lighter, but leaves the count at one segment per step, and proving time follows
   segments. Making steps lighter no longer shortens the chain.
2. **Each step pays a fixed cost unrelated to the step.** It decodes `program.bin`, builds
   `CfsCursor` (`PublicParams::read`, `fraud_proof.rs:70`), verifies the previous journal
   (`verify_previous_journal`, `fraud_proof.rs:497`), verifies the authorization journal
   (`main.rs:48`), decodes its input, and commits a journal that carries the whole `Transition`
   state, including frontiers, the recur stack and the fingerprint accumulator. How much of the
   434–549k user cycles this is has not been measured (step 0). The control-flow guest's setup
   alone was 0.7–0.86M cycles per execution, but that includes a segment decode.
3. **Every step is a receipt.** In real proving, each of the N receipts is lifted, and each
   resolves its own assumptions: the previous transition receipt, authorization, control flow,
   storage and replay. That recursion work is not in the dev-mode numbers above. It grows with N
   no matter how cheap a step is.

The two extracted guests show what segmenting does. The storage guest's fixed work over 32
executions came to 0.17M user cycles more than one execution, and the control-flow guest's
whole-window mode removed most of its per-step cost.

## Goal

1. One transition-guest execution proves **a segment of consecutive window steps**, running the
   same `apply_verified_step` and `finalize` on each, with the state carried in memory between
   them.
2. A segment hands over to the next through today's `Next` journal, unchanged. So `1` is exactly
   today's per-step chain, and `window` is one transition receipt per window.
3. `--transition-segment window|<n>`, next to `--control-flow-segment` and `--storage-segment`.

Non-goals:
- No change to any check or to the order checks run in within a step.
- No change to `TransitionJournal`, `Transition`, `InitTransition`, `StepRecord`, the trace or
  `commit.bin`. A relying party and `chain_fraud` can't tell the modes apart.
- No new guest, and no extraction of another check family.

## Design

### The guest loop

`main` today reads one input, attaches it to the chain (`FraudProofWindowContext::proceed`),
applies it, finalizes it, and commits. It becomes:

```rust
fn main() {
    let params = PublicParams::read();                       // once per segment
    let header: TransitionSegmentHeader = frame::read();     // step_count
    assert!(header.step_count > 0, "a transition segment covers at least one step");
    let state: TransitionState = frame::read();
    let commitment_binding = match &state {                  // as today
        TransitionState::Init(_) => Some((frame::read(), frame::read())),
        _ => None,
    };

    // The first step's input attaches the segment: at `Init` it carries the
    // genesis-only witnesses, at `Next` the previous journal is read after it.
    let first: TransitionInput = frame::read();
    let authorization = verify_authorization_once(&first);   // one env::verify per segment
    let (window_context, mut live) =
        FraudProofWindowContext::proceed(&params, &first, state, commitment_binding);

    let mut inner = InnerReceipts::default();                // §Inner receipts
    let mut current_state = None;
    let mut last_input = None;
    for offset in 0..header.step_count {
        let input = if offset == 0 { first.clone() } else { frame::read() };
        if offset > 0 {
            assert_init_only_fields_absent(&input);           // §Soundness
            assert_same_authorization(&input, &authorization);
        }
        live = live.apply_verified_step(&params, &input, &mut inner);
        let entrypoint = live.entrypoint_authorization();
        let output = live.output_authorization();
        let state = live.finalize(&window_context.init_state.fingerprint,
                                  window_context.final_committed_root.as_ref());
        match state {
            TransitionState::Next(transition) if offset + 1 < header.step_count => {
                live = LiveTransition::resume(&transition, entrypoint, output);
            }
            TransitionState::Finished => assert_eq!(offset + 1, header.step_count,
                "a segment may not continue past the window's last step"),
            _ => {}
        }
        current_state = Some(state);
        last_input = Some(input);                            // `input` dropped each step otherwise
    }
    commit_journal(window_context, current_state.unwrap(), &params, &last_input.unwrap(), …);
}
```

The sketch shows the order of operations, not the final code. Two refinements:
- **Don't round-trip through `Transition` between steps.** `finalize` returns a serializable
  `Transition` because today it is the step's output. Inside a segment, the loop should split
  `finalize` into the fingerprint verdict and `into_transition`, keep the `LiveTransition`, and
  serialize only after the last step. The per-step `resume` above is the literal reading, kept to
  show the semantics are unchanged.
- **Stream the inputs.** Each `TransitionInput` is read as its own frame and dropped after its
  step, as the storage guest does, so peak heap stays at one step's witnesses. The live state that
  crosses steps is small: the trace frontier, `StorageState`, the recur stack, expected
  coordinates and the fingerprint accumulator.

### What is paid once per segment instead of once per step

| work | today | segment of `n` steps |
| --- | --- | --- |
| `program.bin` decode, `CfsCursor::new` | `N` | `N / n` |
| `env::verify` of the previous `TransitionJournal` | `N − 1` | `N / n − 1` |
| `env::verify` of the authorization journal | `N` | `N / n`, with an equality per later step |
| decode and re-encode of `Transition` | `N` | `N / n` |
| journal commit | `N` | `N / n` |
| transition receipts to lift and resolve | `N` | `N / n` |
| `env::verify` of a control-flow / storage journal | `N` each | one per distinct segment journal (§Inner receipts) |
| replay receipt `env::verify` | one per tile step | unchanged |
| the checks themselves | per step | unchanged |

### Inner receipts

Each step verifies the control-flow and storage receipts covering it (`checks::control_flow::
verify_control_flow`, `checks::store::verify_storage`). Inside a transition segment, consecutive
steps usually share one control-flow journal and one storage journal. `InnerReceipts` remembers
the journals already passed to `env::verify` in this execution and skips repeats. Each step still
checks its own entry, path and field equalities, because those are what tie the entry to the step.
Only the `env::verify` call is deduplicated: verifying the same claim twice in one execution adds
nothing.

When all three segment lengths are `window`, a further saving becomes possible: the transition
guest could rebuild each journal's `entries_root` from the entries it was handed and drop the
per-step paths. That is §Deferred. With paths costing about 7k cycles per step
(`control-flow-prover` §Segments), it is not worth a second code path yet.

### Init-only inputs

`TransitionInput` carries four fields that only a window's first step reads, at `Init`:
`window_start_recur_progress`, `entrypoint_membership_witness`, `revealed_tail_roots`, and the
commitment binding frames. Today the host sends `None` on every other step and the guest never
looks at them. Inside a segment, steps after the first are the same kind of step, so the same
rule applies: the guest reads them only from the segment's first input, and only at `Init`.
`assert_init_only_fields_absent` makes that explicit for later steps, so a value present where
nothing reads it is refused rather than ignored. Moving the fields into a segment header would be
tidier, but it changes `TransitionInput`'s shape for no soundness gain, so it is left as an open
question.

### Host

`step_transitions` cuts the window with `SegmentLengths::transition`, the third field next to
`control_flow` and `storage`. For each segment it:
- adds the assumptions of every step in the segment: authorization once, each distinct
  control-flow and storage receipt once, the replay receipts of its tile steps, and the previous
  transition receipt if any,
- writes the params, a `TransitionSegmentHeader`, the state, the `Init` binding or the previous
  journal, then one `TransitionInput` frame per step,
- takes the committed journal's `current_state` as the next segment's state, as it does per step
  today.

CLI: `--transition-segment window|<n>`. The default stays `1` until step 4 measures the modes,
then switches to whichever wins (expected: `window`). *Measured: `window` won on both programs
and is the default.*

### Relation to `transition-split`

It is the same destination reached by a different road. Is the "aggregator" already this?
Partly. In `transition-split`:
- every check family becomes a part guest that runs over the **whole window**,
- an **aggregator** replaces the transition guest. It runs no checks: it verifies one receipt per
  part, equates the parts' column digests, and commits the unchanged `Finished` journal,
- `TransitionState::Next` and `Transition` drop out of the proving path, and chunking a long
  window by resuming from journals is left as open question 2.

That design gets rid of the per-step chain only once all seven parts exist, with the column table,
`Consumed<…>` types and the differential test in place. Control flow and storage were extracted a
different way: as segment guests whose per-step entries each transition step checks against its
own state, with no column digests. This proposal finishes that road. The transition guest keeps
running io, recur, drafts, entrypoint and the trace checks, but over a segment instead of a step.

| | `transition-split` aggregator | this proposal |
| --- | --- | --- |
| who runs the remaining checks | one part guest each | the transition guest, over a segment |
| how extracted guests are consumed | one receipt per part, column digests equated once | per-step entry + path (`ControlFlowEvidence`, `StorageEvidence`), journals deduplicated |
| `Next` / `Transition` | deleted | kept, as the hand-off between segments (`1` = today) |
| prerequisites | all parts, columns, two-pass image-id pinning | none beyond the two extracted guests |
| per-step chain | gone | gone in `window` mode, configurable otherwise |

The two do not conflict. If more families are extracted later as segment guests, the transition
guest is left running fewer and fewer checks over a window. With none left, it *is*
`transition-split`'s aggregator, only consuming entries instead of columns. Doing this first also
answers that proposal's open question 2: a window too long for one execution is cut into segments
that resume through `Next`, which this proposal keeps working.

## Soundness

**Claim:** a segment of `n` steps accepts exactly when the `n` per-step executions it replaces
would all have accepted, and it commits the journal the last of them would have committed.

The per-step chain passes state from step `k` to step `k + 1` by committing `Transition` in a
journal. The next execution then verifies that journal (`verify_previous_journal`), checks
continuity (`assert_state_continuity` and the image-id and manifest continuity asserts), and
resumes. Inside a segment the state never leaves the execution, so each step reads exactly the
value the previous one produced: there is no serialization and no second witness to check. The
continuity asserts are about values the host could vary between executions, namely the image ids,
the program frame and the authorization journal. Within one execution:
- `params` is read once, so the program and image ids can't change mid-segment,
- the authorization journal is verified once, and every later step's copy is asserted equal to
  it,
- `Init` happens at most once per window. The first step of the window's first segment reads the
  `Init`-only inputs, and later steps are refused if they carry any.

**`finalize` keeps its per-step rule.** Every window item before the last must match the
committed fingerprint, and the last must diverge. The loop calls the same verdict on every step.
A `Finished` before the segment's last step is refused, because nothing may follow the window's
last item, and that holds today only because the host stops sending steps. A host that sends
fewer steps than the window holds ends with a `Next` journal, which is not a fraud proof:
`verify_fraud_proof_receipt` requires `Finished`.

**Inner receipts.** Deduplicating `env::verify` changes nothing a step relies on. The claim a
repeated call would assert is already an assumption of this receipt, and the step's entry is still
checked against that journal's root by its own path.

**Between segments** everything is today's mechanism: the `Next` journal, `env::verify` against
the transition guest's own image id, and the continuity asserts. Segment length `1` is
byte-for-byte today's protocol apart from the header frame.

## Cost

*Written before implementation. §Measured after implementation has the numbers.* Not measured. The table in §What is paid once per segment says what moves. What it buys depends
on the fixed share `f` of a step's 434–549k user cycles, which step 0 measures.

- **Segments.** In `window` mode the 32-step chain becomes one execution of roughly
  `32 × (434k − f) + f` user cycles. Even with `f = 0` that is 13.9M user cycles. risc0 cuts it
  into segments of up to 2^20 cycles, about 14–20 depending on paging, against 33 today. With a
  sizeable `f`, it is fewer.
- **Receipts.** 32 → 1, so the recursion work of 31 lifts and 31 resolves of the previous
  transition receipt goes away. GPU proving is where this shows. Dev mode can't.
- **No more chain.** In `window` mode the window becomes three independent proofs (control flow,
  storage, transition) plus the replay receipts. The transition proof depends on the other two
  only through assumptions, so with risc0's unresolved assumptions the three could be proven in
  parallel and resolved at the end (§Deferred).
- **Memory.** Inputs are streamed, so guest heap does not grow with `n`. Host executor memory does
  grow with the segment length. That is why `<n>` stays available, as it does for the other two
  guests.

## Migration

0. **Measure.** Add a temporary `env::cycle_count()` around `PublicParams::read`, the frame reads,
   `proceed` (including the previous-journal verify), each check family in `apply_verified_step`,
   `finalize` and `commit_journal`. Run `hello-tiles --fraud-step 27` and `prompt-prepare
   --fraud-step 796`. Record the fixed share `f`, and per family, so this also settles which check
   would be worth extracting after this.
1. **Loop with `n = 1`.** Restructure `main` around `TransitionSegmentHeader` and the loop, with
   the host always sending `step_count = 1`. Same protocol, new image id. Every existing test and
   the end-to-end windows must pass unchanged.
2. **Segments on the host.** `SegmentLengths::transition`, `--transition-segment`, assumptions
   collected per segment. Add `assert_init_only_fields_absent` and the "no step after `Finished`"
   assert, each with a test that a segment violating it is refused.
3. **Inner-receipt deduplication**, and keep `LiveTransition` across steps instead of
   round-tripping through `Transition`.
4. **Measure every mode** of all three flags on both windows, as `control-flow-prover` §Segments
   and `storage-prover` did. Then pick the default. Run on a GPU if one is free, since receipts and
   recursion are what dev mode doesn't show.

Steps 1–3 each leave a passing suite. `n = 1` remains a valid end state.

## Open questions

1. **Default length.** Expected `window`, decided by step 4. A long window may need a cap, which
   is what `<n>` is for.
2. **Move the `Init`-only fields into the segment header?** It is cleaner, and it would mean
   `TransitionInput` no longer has fields that are meaningful only on one step. It changes the
   input's shape and gains no soundness. Leaning no, unless step 1 makes it the simpler code.
3. **Should the three segment flags collapse into one?** Not yet. The three guests have different
   fixed costs and memory profiles. Revisit after step 4.

## Deferred

- **Parallel proving.** Prove the control-flow, storage and transition segments at the same time,
  passing inner receipts as unresolved assumptions and resolving them at the end. This is
  host-only. It already pays today for storage alongside the chain, and pays more once the chain
  is gone.
- **Rebuilding entry roots instead of paths** when the transition segment covers exactly one
  control-flow and one storage segment.
- **Further extraction** (io folded into the storage guest, then recur) is chosen by step 0's
  per-family split. It no longer affects the chain's length, only the transition segment's size.

## Implementation record (2026-10-21)

Steps 1–3 landed together, with the differences below. Step 0 was not run as a separate probe:
comparing `--transition-segment 1` and `window` on the same windows gives the fixed share
directly (§Measured after implementation).

**Guest.**
- `main.rs` is the loop. Frames are read in this order: params, `TransitionSegmentHeader`
  (`raster-core::transition`), the first step's input, the state, then the `Init` binding or (in
  `proceed`) the previous journal, then each later step's input. This is today's order with the
  header and the later inputs added, so the host change is small.
- `LiveTransition::finalize` is split into `reaches_window_end`, the per-step fingerprint verdict
  run after every step, and `into_state`, run once after the last step. The live state is kept
  across steps, never round-tripped through `Transition`. Its doc no longer claims the first step
  is "committed without comparison": the code has compared every item since the first-step
  exception was removed, and `FraudProofWindowContext`'s doc already said so.
- **Authorization** is verified once per segment, from the first step's input.
  `assert_same_authorization` holds every later step's image id and **whole journal** equal to it,
  not only the manifest commitment, because each step's entrypoint check reads its own copy.
- **Init-only inputs.** `assert_init_only_inputs_absent` runs on every step except the window's
  opening one. That includes the first step of a `Next` segment, which today's guest also never
  read, so a `Next` execution now refuses them too, at any segment length.
- **A step after `Finished`** is refused ("a transition segment may not continue past the window's
  last step").
- **Inner receipts.** `checks::receipts::VerifiedJournals` holds the (image id, journal bytes) pairs
  already passed to `env::verify` in this execution. `verify_control_flow` and `verify_storage`
  take it, and still check each step's entry, path and fields.

**Host.** `step_transitions` cuts the window by `SegmentLengths::transition`. Per segment, it adds
the authorization receipt once, each distinct control-flow and storage receipt once (deduplicated
by journal bytes), each tile step's replay receipt, and the previous transition receipt. The
`Init`-only inputs now go on the window's first step, keyed by step index rather than by "no
journal yet", because the whole first segment has no journal yet. `replay_receipt` is factored out.
CLI: `--transition-segment`, default `window`. Chain evidence uses `SegmentLengths::default()`,
which is `window` for all three.

**Tests.** Transition guest, 158: seven new tests in `segment_inputs`, for the two asserts and
their accept cases. That also gives the long-unused `authorization_journal` test helper its first
caller. The loop itself, the `Finished` assert and the journal deduplication run only in the zkVM.
They are covered by the end-to-end windows below and by the in-zkVM `transition_guest_*` host
tests, which now send a one-step header. All suites pass: the storage (17), control-flow (55) and
transition (158) guests, `raster-core`, `raster-prover` (53, dev mode) and `raster-cli`.

**What moved.** The transition guest's image id, and its host input protocol (the header frame).
`TransitionInput`, `Transition`, `TransitionJournal`, `StepRecord`, `commit.bin`, the control-flow
and storage guests, and the tile ids did not. Both programs rebuilt from this checkout write a
lock byte-identical to their previous one. The `raster-core` edit only forced a one-time tile
rebuild, because the tile cache is keyed on the source hash.

**End to end.** `hello-tiles`, 32-step windows: `--fraud-step 27` in `1`, `8`, `window` and
all-`1` modes, plus `47` (recur-sequence body) and `58` (nested) in the default mode.
`prompt-prepare --fraud-step 796` in `1`, `8` and `window`. Every run generated a fraud proof, and
`verify_fraud_proof` accepted the `hello-tiles` whole-window proof with all four guests pinned.

## Measured after implementation (2026-10-21)

Dev mode, risc0 `SessionStats` per proof (temporary probe, not committed). The same windows as
`storage-prover` §Measured after implementation. Control flow and storage are `window` unless
stated. Cells are user cycles / total cycles / risc0 segments, summed over the window.

`hello-tiles --fraud-step 27`, 32 steps:

| transition segment | transition guest | all guests |
| --- | --- | --- |
| before `storage-prover` (`0474a76`) | — | 48.53M / 66.58M / 73 |
| `1` (the per-step chain) | 14.09M / 31.20M / 33 | 47.94M / 72.35M / 73 |
| `8` | 6.99M / 10.09M / 11 | 40.83M / 51.25M / 51 |
| `window` (default) | **6.23M / 8.39M / 8** | **40.08M / 49.55M / 48** |
| all three guests at `1` | 13.59M / 30.67M / 33 | 52.13M / 90.70M / 118 |

`prompt-prepare --fraud-step 796`, 16 steps:

| transition segment | transition guest | all guests |
| --- | --- | --- |
| before `storage-prover` (`0474a76`) | — | 29.29M / 39.58M / 39 |
| `1` | 8.89M / 17.33M / 18 | 29.44M / 42.24M / 43 |
| `8` | 4.99M / 6.82M / 7 | 25.53M / 31.72M / 32 |
| `window` (default) | **4.69M / 6.29M / 6** | **25.24M / 31.20M / 31** |

Also `hello-tiles` `47` and `58` in the default mode: one transition receipt each, 7.06M / 9
segments and 7.12M / 9.

- **The fixed share, step 0's question.** Spread over the 31 and 15 executions it saves, `window`
  removes 254k (`hello-tiles`) and 280k (`prompt-prepare`) user cycles per step, out of 440k and
  556k. **More than half of a per-step transition execution was overhead**: program decode,
  `CfsCursor`, the previous-journal and authorization verifies, the input and state codecs, and
  the commit.
- **The transition guest:** user −56% and −47%, total −73% and −64%, segments 33 → 8 and 18 → 6.
  Total cycles fall further than user cycles because 32 small executions each paid paging and
  padding up to a full segment.
- **The window:** against the per-step chain, total cycles −32% and −26%, segments −34% and −28%.
  Against `0474a76`, before both this and `storage-prover`: total −26% and −21%, segments 73 → 48
  and 39 → 31. On one GPU, where proving time follows segments, that is the first change in this
  series that should make a window faster rather than only restructure it.
- **No more chain.** In `window` mode a window is three receipts (control flow, storage,
  transition) plus the replay receipts, and the storage guest is now the largest (36 and 22
  segments). §Deferred's parallel proving would run the three side by side.
- **`8` is close to `window`** (11 vs 8 segments, 7 vs 6), so a cap for very long windows costs
  little.
- **`1` is one segment more than before on `prompt-prepare`** (18 vs 17), from the header frame
  and the per-step asserts. It is kept for comparison and debugging.

**Not measured:** real GPU proving with recursion. Dev mode shows segments but not the 31 lifts
and resolves the per-step chain also paid, so the GPU gain should be larger than the segment
counts say.
