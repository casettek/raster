# Proposal: `recur-state-chaining` — bind iteration *N*'s carried state to iteration *N−1*'s output

Status: **implemented** 2026-09-11. Split out of
[`loop-carried-state.md`](./loop-carried-state.md) §4 / §Migration step 1, which says this fix
"should not wait for the rest" of that proposal — it is a soundness fix over today's programs,
where the rest is a new representation.

Related:
- [`loop-carried-state.md`](./loop-carried-state.md) — the parent. Its by-reference
  `RecurSequenceStateRef` remains unimplemented and unscheduled; this closes only the chaining
  gap its §4 describes.
- [`recur-progress-commitment.md`](./recur-progress-commitment.md) — the channel this extends.
  The carried state became one more frame field rather than a new carrier.
- [`window-seed-reconstruction.md`](./window-seed-reconstruction.md) — why window opening cost
  nothing here: the seed is a `RecurProgressStack`, passed whole, so a new frame field rides along.
- [`carried-state-channel.md`](./carried-state-channel.md) — argues against a fourth carrier.
  Honoured: no `TrackedStateRoot` map was added.

## Problem

A `call_recur!` / `call_recur_seq!` site carries loop state between iterations, and **nothing in
the trace recorded that the state entering iteration *N+1* was the state that left iteration *N***.
The threading is a plain in-memory Rust loop variable (`raster/src/input.rs:2705`); that identity
existed only in host memory.

In the trace the state travels as `FnInputValue::Inline`, and the transition guest's entire
obligation for an inline binding is that the source *is* inline
(`guests/transition/src/checks/cfs.rs`):

```rust
InputBinding::Direct(InputSource::Inline) => {
    assert!(matches!(resolved_source, ResolvedSource::Inline(_)), ...);
}
```

It checks the kind, never the bytes. A claimed trace could substitute arbitrary state between
iterations and still verify. The codebase already names the cost
(`raster-compiler/src/flow_resolver.rs:583`): *"Binding it as `Inline` would let a claimed trace
substitute arbitrary bytes for it and still verify."*

The `output` slot had no such gap — `DraftReplayTransition.root_before` is replay-proven and
chained. `loop-carried-state` §Problem: *"the asymmetry is a bug, not a design."*

## Three defects found while implementing, all fixed here

**1. No recur sequence was provable at all.** The recur-sequence *site* recorded one input value
(`raster-macros/src/recur.rs`) while `RecurSequenceItem.sources` holds one binding per
`call_recur_seq!` argument, and `verify_step_record_inputs` asserts the two arities agree. A site
step — unlike an iteration step — does not short-circuit before that assert. Every recur sequence
in both repos would have failed it.

It went unnoticed because `verify_step_record_inputs` has exactly one caller, inside the guest,
and `prove()` runs only when `verify()` returns `Fraud` (`raster-cli/src/commands/run.rs`). An
honest `--commit`/`--audit` never executes it. The guest tests contained zero mentions of
`RecurSequence`.

**2. `FnCallRecord.recur_control` was never set.** All 31 initializers were `None` and the
recorder folded `Continue` via `unwrap_or`. The field was added by `recur-progress-commitment`
§3.1 and never wired, so any `Break`-terminated sweep would fold the wrong control. Now set by
the tile wrapper, and the recorder panics rather than defaulting.

**3. A stateful recur sequence had no test anywhere.** All three `call_recur_seq!` sites in
`raster` are output-only; `raster-inference` has four stateful ones
(`output-finalize`, `prefill-range` `project_token`, `prompt-prepare` ×2), which is why the shape
is load-bearing and closing it off was not an option.

## Design

`RecurProgressFrame` gains `state_commitment: Option<Hash32>` — the commitment of the carried
state after the last iteration. It folds through the same `advance` the rest of the frame uses,
is carried by the same per-step `recur_progress_commitment`, and needs no new seed plumbing.

`RecurStateTransition { state_in, state_out }` is recorded in three places, each for a consumer
that cannot see the others:

| carrier | who reads it | bound by |
| --- | --- | --- |
| `RecurTileReplay.state` | the guest, for tiles | replay proof — `env::verify` over a registry-pinned image |
| `FnCallRecord.recur_state` | the recorder | it is the producer's own copy |
| `StepRecord.recur_state` | the guest, for sequences | trace fingerprint, plus the two asserts below |

Duplication is only safe where an equality makes the duplicate non-load-bearing, so the guest
asserts both:

- a tile's step-record copy **equals** its replay-proven copy;
- any iteration's `state_in` **equals** `state_commitment` of the state it actually recorded
  reading (`input_source_witness.values()[1]`, already bound by `input_source_commitment`).

The fold rule, with no `if let Some` — absence is the cheapest attack on a continuity check, and
is what `checks/drafts.rs` still permits for drafts:

| frame | incoming | action |
| --- | --- | --- |
| `None`, first fold | `Some(t)` | adopt `t.state_in`, then set `t.state_out` |
| `Some(h)` | `Some(t)` | require `t.state_in == h`, then set `t.state_out` |
| `Some(_)` | `None` | `CarriedStateOmitted` |
| `None`, not first fold | `Some(_)` | `CarriedStateUnexpected` |

`is_first` is **passed, not inferred**: a tile folds before its iteration counter moves, a
sequence after. Inferring it from `next_iteration_index` made a stateless site's second iteration
look like a first one — precisely the case that must be rejected.

Tiles fold inside `advance_tile_iteration`, which has the transition and the position together.
Sequences count at the iteration's `Start` and fold at its `End`, via
`fold_sequence_iteration_state`, because what an iteration *produced* is not known until it
closes.

### What anchors what

Every `state_in` is bound to the step's own recorded input. Every `state_out` but the last is
pinned by the next iteration's bound `state_in` through the fold rule.

The **last** one has no successor, and is pinned differently per kind:

- a recur **tile**'s final `state_out` is computed inside the replay entry point, so it is
  replay-proven like the rest of the journal;
- a recur **sequence** has no journal, so `RecurTileItem`/`RecurSequenceItem` gain
  `state_is_output` — true when the call passes `state` and no `output`, i.e. when the site's own
  result *is* its carried state. For such a site, each iteration's `state_out` is checked against
  the bytes that iteration actually emitted.

That check lives on the **iteration**, not on the site's close, and the reason is an encoding
mismatch that cost a wrong first attempt: a site's recorded output is the *raster-encoded stored
object*, while the carried state is *postcard*. Comparing the chain against the site's output
compares two encodings — it failed on `hello-tiles`' state-only recur immediately. An iteration's
recorded output is postcard, the same form the state is committed in, so that is where the two
coincide.

A state+output site is deliberately exempt: it returns the draft, not the state, so its output is
not the carried state and must not be held against it.

## Non-goals, stated because the guarantee is narrower than it sounds

**Iteration 0's seed is not pinned to the value the program wrote.** `InputSource::Inline` is a
unit variant — the CFS holds no literal bytes — and every seed in both repos is a literal. The
guarantee is *"the fold is consistent with the seed the prover recorded"*, not *"with the seed the
program wrote"*. Pinning the latter needs `InputSource::Inline → InlineLiteral { commitment }`
across every inline argument on every tile, which is a broader change.

**A state+output recur sequence's final `state_out` is still unpinned.** Such a site discards its
carried state and returns the draft, so there is no recorded value to hold the last state against.
Every *earlier* state in the sweep is pinned by its successor, so this is a terminal gap, not a
substitution inside the loop — and the state is still bound per-iteration by `state_in` against
what the iteration read. Closing it would mean recording the produced state separately from the
step's output; not done.

## What changed

| file | change |
| --- | --- |
| `raster-core/src/draft.rs` | `RecurStateTransition`; `RecurTileReplay.state` |
| `raster-core/src/trace.rs` | `FnCallRecord.recur_state`; `StepRecord.recur_state` |
| `raster-core/src/cfs.rs`, `raster-compiler/src/{ast,flow_resolver}.rs` | `state_is_output` on both recur items, from `state` present and `output` absent at the call |
| `raster-core/src/recur_progress.rs` | `RecurProgressFrame.state_commitment` and `state_is_output`; `fold_carried_state`; `fold_sequence_iteration_state`; three `RecurProgressViolation` variants; `state_commitment()` — one implementation, three callers |
| `raster-macros/src/lib.rs` | `gen_recur_state_start` / `gen_recur_state_finish`, used by both the native and replay wrappers; `gen_native_recur_control_capture` — the fix for defect 2 |
| `raster-macros/src/recur.rs` | the recur-sequence **site** records every declared source (defect 1); the recur-sequence **step** captures its transition |
| `raster-runtime/src/tracing/recorder.rs` | folds the host copy for both kinds; `recur_control` no longer defaults |
| `guests/transition/src/checks/cfs.rs` | folds the replay-proven copy for tiles and the step-record copy for sequences; the two binding asserts |

## Verification

- `raster-core` 141, `raster-compiler` 39, `raster-runtime` 63, `raster` 71, transition guest 56 —
  all passing.
- Guest: a substituted iteration state, an omitted one, an unexpected one, and a seed differing
  *only* in carried state (which must commit differently) are each rejected; an honest chain
  advances.
- Guest: a state-returning sequence iteration whose recorded output is not the state it claims to
  have produced is rejected, as is one with no output to check against; a state+output iteration
  is correctly exempt.
- Guest: a recur-sequence site recording only its input is rejected on arity — the defect-1
  regression — and one recording every declared source verifies.
- `raster`: a stateful recur sequence threads its state, records three site values, and its
  iterations chain (`state_out(i) == state_in(i+1)`).
- `hello-tiles` runs, `--commit`s and `--audit`s clean on the new trace format.

**Not run:** an end-to-end fraud-proof window over a recur sequence. That path drives the real
RISC0 prover and the repo has no `RISC0_DEV_MODE` convention to make it cheap — the same gap
`window-seed-reconstruction` §Implementation record records. The guest logic is covered natively;
what is untested is the plumbing that reaches it.

**Not run:** `raster-inference` acceptance. Its four stateful recur sequences are the reason this
work exists, and the site now resolves its storage-backed arguments once per call
(`auth_ref_trace`, memoised by `THREAD_AUTH_REF_TRACE_MEMO`). For `project_token`, whose args
include three `List<BytesPage>`, that memo is assumed to absorb the cost. Unmeasured.

## Format break

Every program re-locks. `program_commitment` moves because `ProgramDefinition` contains the tile
image ids and the replay entry point emits new journal fields. The trace format, every
`recur_progress_commitment`, the trace event format and both guest image ids move with it. Input
fixtures are unaffected — no schema or input encoding changed.
