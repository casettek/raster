# Issue: `recur-carried-state-unbound` — a recur site's carried state is chained to itself, not to the computation or to the result the site stores

Status: open 2026-09-28. **Soundness gap in shipped code**, live in `raster-inference`. Direction
picked, not implemented:
[`incremental-draft-materialization`](../proposals/incremental-draft-materialization.md)
§Carried-state commitment (decision D5b) and §Sequence return binding (D5c).

Related:
- [`recur-state-chaining`](../proposals/recur-state-chaining.md) (implemented) — chains every
  `state_in` to the previous `state_out`. Its §What anchors what (`:113`) says a recur tile's final
  `state_out` is replay-proven; that is true of the *chain*, and nothing connects the chain to the
  object the site stores. It records trying a site-level comparison and abandoning it over an
  encoding mismatch.
- [`program-output-unbound`](./program-output-unbound.md) — the same missing return binding, for
  storage-backed values. This issue is its inline-state half, plus the site-result gap below.
- [`recur-accumulator-slots`](./recur-accumulator-slots.md) — about *where* loop state can live
  cheaply; not about binding it.

## What happens

Two gaps, one for each end of the chain.

**1. A state-only site's stored result is not its final state** (recur tiles and recur sequences).
A state-only site stores its final `T` as its result: `bind_infallible_call(state.into_inner())`
(`raster/src/input.rs:2726`, and `:2625`, `:2877` for the other state drivers) →
`store_execution_output_value` (`raster-runtime/src/storage.rs:1528`) at
`current_recur_site_coordinates()` (`:1531`). The recorder writes it at `[s]` with
`output_commitment` = its raster root (`raster-runtime/src/tracing/recorder.rs:1043`). The chain
ends at `frame.state_commitment = H("recur-carried-state" ‖ postcard(T))`
(`raster-core/src/recur_progress.rs:591`). No step compares the two: `close_site` (`:517`) takes no
output, and the I/O check skips execution steps (`checks/io.rs:33`). They also *cannot* be compared
directly — different hash functions over the value.

```
iterations:  {0} → {1} → {3} → {6}      chain proven: final state_out = H(postcard({6}))
site close:  writes {7} at [s]          output_commitment = raster_root({7})   ← accepted
```

Every later reader of `[s]` consumes `{7}`, and its own checks pass.

**2. A recur sequence's state is not tied to what its body computed.** A sequence is not replayed.
Its step function computes `state_out` host-side from the value the body returned
(`raster-macros/src/recur.rs:1011`). The guest binds `state_in` to the iteration's own inline input
bytes (`checks/cfs.rs:870`, `:889`) and, for a state-only site, `state_out` to the iteration's
`SequenceEnd` output bytes (`fold_sequence_iteration_state`, `recur_progress.rs:482`). Those bytes
are checked against nothing the body computed: `SequenceDef` has no return binding (see
`program-output-unbound`). So the `state_out`s form a consistent chain whose values are free.

## Reproduce

By reading the code cited above; not probed. The shortest probes: (1) a state-only recur tile site
whose `RecurTileEnd` output differs from the last iteration's returned state verifies; (2) a
stateful recur sequence whose iteration `SequenceEnd` output differs from its body tile's output —
with the next iteration's inline state input changed to match — verifies.

## Where it is live

`raster-inference`'s `prefill-range` `attend_token` (`prefill-range/src/main.rs:98-160`) chains
eight **state-only recur tiles** through their stored results — `scores = call_recur!(…, state =
scores)`, each seeded by the previous site's result, then `context_acc` the same way — so gap 1
applies at every link. `output-finalize` runs a state-only **recur sequence**
(`decode_generated_token`, `output-finalize/src/main.rs:29`) whose final state is then selected for
the program's output, so gap 2 reaches a program output.

## What it is not

- Not the iteration-0 seed: `recur-state-chaining` §Non-goals already states that an inline literal
  seed is unpinned. A *stored* seed is a different matter and is pinned by the picked direction.
- Not the per-iteration write of the state at `[s][i]`: that is a cost, not a binding, and is
  removed separately (incremental-draft-materialization D3).

## Directions

Picked, in `incremental-draft-materialization` §Carried-state commitment (D5b), with §Sequence
return binding (D5c): commit a carried state by its object commitment — the raster root of the
inner `T` — computed in the replay for tiles; pass a recur sequence's state by reference, so
`state_in` is the commitment of the reference the iteration's `Start` binds and `state_out` that of
its returned binding, which D5c ties to a body tile's output; check a state-only site's result at
its close, `frame.state_commitment == output_commitment`; and open the frame from a stored seed's
binding. Requires the replay's raster root to equal storage's for every state type — untested
beyond one struct.
