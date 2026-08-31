# Issue: `chain-shape-count-unverified` — a run directory's recorded count answers for a producer nobody re-reads

Status: open 2026-08-31. Unowned.

Reproduced against `feature/chain-stage-repeat` at `5f013e7`. Every citation is committed code.

Related:

- [`chain-repeat.md`](../proposals/chain-repeat.md) §4 (l.401-425) — **owns the sidecar, and its
  scope stops one step short of this.** It decides that `chain_dir/chain-shape` exists, that it is
  written unconditionally, and (as of `5f013e7`) that it carries `spec_digest` so a count resolved
  from one manifest cannot be inherited by another. That binding answers *which manifest* the
  count belongs to. It does not answer *whether the count is the one the producing stage
  committed* — the sidecar is the only witness to its own number.
- [`chain-io-commitment.md`](../proposals/chain-io-commitment.md) §3 (l.266) — owns `--stage` as
  the dispute path: a contested stage is re-run authenticated **in place** to produce the trace a
  fraud proof is built from. It specifies what that re-run must produce. It does not specify what
  places the stage in the graph, which under a repeat block is a resolved count.
- [`chain-stage-execution.md`](../proposals/chain-stage-execution.md) §3 (l.149), §4 (l.178) —
  owns rehydration and downstream invalidation. Both take the expanded spec as given; §4's
  over-delete-rather-than-under-delete argument is about *which* stages follow the re-run one, not
  about whether the list is the right list.

## 1. What exists, mechanically

On a `--stage` re-run, the stage list is expanded from counts taken out of the run directory's
`chain-shape` sidecar:

- `read_chain_shape` (`raster-cli/src/chain.rs:748`) decodes the sidecar as a `ChainShape`,
  refuses it whole if `spec_digest` does not match the manifest in hand (`:755`), keeps the
  resolutions whose `source_stage` is `Some` (`:771`), and returns `(width, count, source_stage)`
  per block name.
- `run` seeds `known` from that, and only for `--stage` (`chain.rs:406-411`).
- `check_recorded_count` (`chain/expand.rs:264`) checks the count against the manifest's `max`
  (`:271`) and the recorded `source_stage` against the position `producer_index` derives for the
  block's `from` (`:278`).

Those are the only checks. Both are internal to the record: `max` and the producer position are
re-derived from the manifest, and the manifest is what the digest already pinned. **The count
itself is compared against nothing.** A sidecar whose `resolved_count` has been altered to any
other value ≤ `max`, leaving `source_stage` alone, is accepted and expanded.

The artifact that would settle it is in the same directory and already has a decoder.
`read_trip_count` (`chain.rs:806`) reads `chain_dir/<from>/output.bin`, parses the one scalar leaf
(`raster_core::input::parse_scalar_leaf`) and applies `max` itself. It executes nothing — it is a
file read. It is called from exactly one place: the whole-chain partition loop at `chain.rs:626`,
immediately after the producing stage runs. Nothing calls it on the `--stage` path.

The asymmetry with verification is the sharp part. `verify_shape` (`chain/expand.rs:322`) derives
each count from the **producing stage**: it re-encodes `resolved_count` at the recorded width and
compares against `producer.output_structural_commitment` (`:482-491`). A verifier never takes a
count on the record's own word. The run loop, on the `--stage` path, does.

## 2. The gap

A `--stage` re-run against an altered-but-plausible count expands a stage list that disagrees
with what the producing stage put on disk, then:

1. **executes** the selected stage against inputs synthesized from that list
   (`synthesize_inputs`, `chain.rs:2009`);
2. **deletes** every stage directory that follows it *in that list*
   (`invalidate_downstream`, `chain.rs:1808`);
3. **rewrites** the sidecar with the same count and the current digest
   (`write_chain_shape`, `chain.rs:661`), which runs before every early return below it;
4. **leaves the chain-commitment untouched** (`chain.rs:682`), by design — so nothing in the
   directory afterwards records that the artifacts and the commitment now describe different
   chains.

Deflation is the silent direction. `examples/chain-example/Raster-dynamic.toml` exports
`steps.total` from `step{t}` at the block's last iteration; with the sidecar reading 2 where the
planner committed 4, the sink binds to `step1` instead of `step3`. `step1/output.bin` exists, so
`synthesize_inputs`' missing-producer refusal (`chain.rs:2048`) never fires, and `chain run
--stage sink` re-runs the sink over the wrong producer, writing an `input_manifest.json` and a
`commit.bin` for it. Under `chain-io-commitment` §3 that `commit.bin` is the dispute artifact for
the sink — a trace of an execution the recorded chain does not claim. Inflation fails loudly
instead (`step4` has no directory), which is why the two directions are worth stating separately.

### The same divergence with nothing tampered

`chain run --stage <the count producer>` — re-running the planner itself, which is what a dispute
over the planner requires — produces the divergence from an honest directory:

1. expansion places the graph from the sidecar's count `K` (`chain.rs:406-412`);
2. `invalidate_downstream(.., 0)` deletes every step and the sink;
3. the planner runs and writes a new `output.bin`, count `K'`;
4. `write_chain_shape` (`chain.rs:661`) records `K` — `repeats` came from the expansion in (1),
   and nothing between (3) and (4) re-reads the artifact (3) just wrote.

The directory then asserts `K` while its planner's artifact says `K'`, with a matching digest and
no edit to any file by hand. With the repo's fixture planner `K' == K`, because it is
deterministic and its input did not change; the point is that no code path establishes that. A
planner whose output moves — a changed `budget` input, a rebuilt program — makes it observable.

## 3. What this is not

- **Not the manifest-binding gap.** `5f013e7` closed that one: a sidecar resolved from a different
  manifest is discarded (`chain.rs:755`) and reported as such. This issue is what remains when the
  digest matches — the record is for the right chain and still unverified.
- **Not a hole in `verify_shape` or in the commitment.** `chain audit` re-derives counts from the
  commitment's own checkpoints (`chain/expand.rs:482-491`) and is unaffected. What the commitment
  does not cover is the run directory's *other* copy of the shape, and the artifacts a `--stage`
  run rewrites against it.
- **Not reachable from a whole-chain run.** Those never read the sidecar (`chain.rs:406-411`) and
  resolve every count through `read_trip_count` (`:626`).
- **Not, on its own, an argument about who can write a run directory.** An attacker with write
  access to `chain-shape` also has it to `planner/output.bin`. The observation is narrower: the
  artifact is the thing a checkpoint commits to and a verifier re-derives from, and the sidecar is
  a second number nothing else references — so the two are not equally corruptible in effect. §2's
  second half needs no attacker at all.

## 4. Directions, none chosen

1. **Re-derive on the `--stage` path instead of reading counts back.** Run the same
   partition loop `run` already runs, resolving each pending count with `read_trip_count`
   (`chain.rs:806`) against the producer's existing `output.bin` rather than by executing it.
   Costs: makes the sidecar unnecessary as an *input*, which contradicts §4's stated reason for
   its existence (l.401-405: a stage-produced count "cannot be re-derived without re-running its
   producer" — `read_trip_count` is a file read, so this is false of a directory whose producer
   has already run). §4 would need amending, and a decision on whether the sidecar keeps being
   written as a record. Does not fix §2's second half: a re-run of the producer still writes a
   shape resolved from the pre-run artifact.
2. **Cross-check the sidecar against the artifact, keep both.** Read the producer's `output.bin`
   where it exists and refuse a disagreement. Costs: two sources of truth kept in step by a third
   piece of code, and it has to decide what an *absent* artifact means — silently trusting the
   sidecar there re-opens exactly this issue for the case where downstream invalidation has
   already deleted the producer.
3. **Check against the chain-commitment when the directory has one.** `ChainShape.repeats` there
   is verifiable (`verify_shape`, `chain/expand.rs:322`). Costs: dead for the case the sidecar was
   introduced to serve — `run` degrades to no commitment when program identity is unresolvable,
   which §4 (l.402-405) names as precisely the state a contested chain can be in.
4. **Refuse `--stage` on a stage that produces a count.** Answers §2's second half directly: the
   shape used to place stages is invalidated by the run itself. Costs: that stage is exactly the
   one a dispute over the planner needs re-run authenticated (`chain-io-commitment` §3), so this
   removes the dispute path for it unless paired with re-resolving the shape afterwards — at which
   point every downstream artifact was invalidated against the old count anyway.

Directions 1 and 4 answer different halves and are not exclusive. All four turn on a question §4
did not have to ask when the sidecar was only about placing names: **is the run directory's
recorded shape a cache of something re-derivable, or the record of something that is not?** The
code currently treats it as the second; `read_trip_count` is evidence for the first.

## 5. Reproducing

```bash
# the read path: digest + source_stage, and nothing about the count itself
sed -n '748,784p' crates/raster-cli/src/chain.rs
sed -n '256,290p' crates/raster-cli/src/chain/expand.rs      # check_recorded_count

# the decoder that is never called here, and its one caller
sed -n '798,840p' crates/raster-cli/src/chain.rs             # read_trip_count — a file read
grep -n 'read_trip_count' crates/raster-cli/src/chain.rs     # 626 only, the whole-chain loop

# what a verifier does instead, for contrast
sed -n '470,495p' crates/raster-cli/src/chain/expand.rs      # count re-derived from the producer

# the three writes a --stage run makes against the expanded list
grep -n 'invalidate_downstream\|write_chain_shape\|left untouched' crates/raster-cli/src/chain.rs
```

The tampering half, end to end — the sidecar is postcard `ChainShape`, so `resolved_count` can be
moved without touching the digest:

```bash
cd examples/chain-example
cargo raster chain run Raster-dynamic.toml --no-auth
# lower chain-shape's resolved_count by two (keep source_stage and width), then:
cargo raster chain run Raster-dynamic.toml --no-auth --stage sink
# the sink re-runs bound to step1 rather than step3; chain-commitment still says step3
```

The half that needs no tampering:

```bash
cargo raster chain run Raster-dynamic.toml --no-auth
cargo raster chain run Raster-dynamic.toml --no-auth --stage planner
# chain-shape is rewritten from the expansion that preceded the run, not from the
# output.bin the planner just wrote — identical here only because the fixture planner
# is deterministic and its input did not change
```
