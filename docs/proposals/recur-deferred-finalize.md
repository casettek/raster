# Proposal: `recur-deferred-finalize` — a recur that hands its draft back

Status: **implemented** (2026-08-28), behind an opt-in `finalize = false`.

Companion to: [`incremental-draft-witness.md`](./incremental-draft-witness.md) (implemented) — which
made an output draft pay only its increment, and is what makes a second writer affordable.

Motivating program: `raster-inference`'s multi-token decode. See that repo's
`docs/issues/two-recurs-one-draft.md` for the failure this fixes and
`docs/proposals/multi-token-decode.md` for what it unblocks.

## Problem

Every recur driver finalizes its output draft and returns `AuthRef<S>`
(`raster/src/input.rs`: `run_recur_list`, `run_recur_chunked_list`,
`run_recur_chunked_list_with_state`, `run_recur_list_with_state`, `run_recur_sequence_list`,
`run_recur_sequence_list_with_state`). Nothing converts a finalized value back into a draft —
`IntoDraft` is implemented only for `Draft<S>` and `RecurSequenceOutput<S>`, both already open.

So **one draft admits one recur**, and an undocumented shape rule follows:

> Every list in a stage output built by a recur is built by the *same* recur, over the *same*
> input list — so they all have that list's length.

A `call!` chain may thread a draft freely; the first recur ends it. `raster-inference`'s
`prefill-range` satisfies the rule by accident (`finish_layer` pushes one activation row and one
K/V row per query, so both lists are `queries.len()`).

Decode breaks it. A decode stage processes **one** token and must publish a K/V cache holding
every prior token plus that one:

```
ActivationSequence { rows: 1 entry, kv: prompt_len + t + 1 entries }
```

Two lists, two lengths, two recurs, one draft. Not expressible. The cache is not optional: stage
`t`'s `kv` is stage `t+1`'s `prior_kv`.

## Why this is cheap — attestation does not live at `finalize`

The question that looks hard is *what a partially built draft commits to between recurs*. The
answer is already in the code, and it is: **the same thing it commits to between tiles.**

`verify_draft_transition` (`raster-prover/guests/transition/src/checks/drafts.rs`) attaches draft
witnesses to **`TileExec` steps**, not to the recur wrapper. Each step that mutates a draft carries
a `DraftReplayTransition { draft_id, schema_hash, root_before, ops }`; the guest checks the witness
root against `root_before`, re-applies the ops, and asserts **root continuity** against
`active_drafts`. Every append inside a recur is already attested one iteration at a time, and the
chain of roots already continues across step boundaries.

`finalize` materializes a value and binds an `AuthRef`. It is not what makes the appends sound.
Deferring it therefore moves no attestation — it moves only the materialization.

## Design

An opt-in flag on `call_recur!`, placed before the mandatory-last `args`:

```rust
let draft = call!(begin_layer_output, new!(ActivationSequence), start_position);
let draft = call_recur!(
    tile = carry_cached_key, input = prior_keys, chunk = 64,
    output = draft, finalize = false, args = (start_position, sliding_window)
);
let out = call_recur_seq!(sequence = attend_token, input = queries, output = draft, args = (..));
```

**Opt-in, not inferred.** An earlier sketch had the macro return `Draft<S>` whenever `output` was an
already-open draft rather than `new!(T)`. That is wrong: `raster-inference`'s
`prefill-finalize/src/main.rs:34-42` is exactly that shape today and *relies* on finalization,
selecting `.errors` off the result. Inferring intent from the output expression would silently
change it.

**A bool literal, for the same reason `chunk` is an integer literal.** Whether a recur closed its
draft is a fact about the program's shape, so it is pinned in the CFS rather than decided at run
time.

`finalize = false` on a recur with no `output` is rejected at parse time: a state-only recur has no
draft to leave open.

### Mechanism

1. **Drivers.** Each of the six output-bearing drivers becomes `*_with_finish`, generic over
   `Finish: FnOnce(Draft<S>, bool) -> R`. Two thin wrappers per driver: the existing name passing
   `finalize_recur_output`, and a `*_open` passing `|draft, _| draft`. No loop body is duplicated
   and no existing signature changes.
2. **Macro.** `gen_recur_step_function` emits a second hidden entry point,
   `__raster_recur_auth_open_<tile>`, for output-bearing modes only. Same sweep, same tracing, one
   difference: it cannot resolve a finalized value for `RecurTileEnd`, so it records
   `output: None`. The iteration-level draft transitions carry the attestation, as above.
3. **Call site.** `finalize = false` routes to the open entry point.

## What this deliberately does not change

- **Drafts stay linear and set-once.** Deferring moves the single `finalize` to the end of the
  chain of writers; it does not permit two writers at once, or a second `.set()`.
- **The default.** Omitting `finalize` closes the draft, exactly as before. Every existing program
  compiles and behaves identically — checked by `cargo check --workspace` over the examples, and by
  `raster-inference`'s tiny-model chain producing bit-identical logits.
- **Sequences.** Only `call_recur!` takes the flag. `call_recur_seq!` is the natural *closer* in
  the motivating program, and adding an unused open path to it would be speculative.

## Open questions

- **Should the CFS distinguish an open recur explicitly?** It currently does so implicitly, via
  which entry point the site calls. An explicit field would let an audit reason about "this draft
  had N writers" without re-deriving it from the call graph.
- **Is `output: None` on `RecurTileEnd` the right record**, or should an open recur emit the
  draft's post-root so a reader can see the sweep advanced something? The latter duplicates what
  the per-iteration transitions already carry, which is why it is not done here.
