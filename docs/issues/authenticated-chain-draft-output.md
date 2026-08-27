# Issue: `authenticated-chain-draft-output` — a draft-returning stage cannot complete an authenticated chain run

Status: open 2026-08-27. Unowned.

Related:
- [`chain-stage-execution.md`](../proposals/chain-stage-execution.md) — **supplies the fixture
  that exposes this and the reason it was never seen.** Its §Verification landed
  `crates/raster-cli/tests/chain_stage_cli.rs`, whose `Fixture::chain` (`:52-61`) hard-codes
  `chain run --no-auth`. Its own §Not covered admits the gap: *"posture isolation (no `--stage`
  invocation touching `target/raster/chains/`)"*. So no test has ever run this chain
  authenticated.
- [`chain-io-commitment.md`](../proposals/chain-io-commitment.md) — found it, does not own it.
  That proposal's V4 (a cheap and an authenticated `chain run` must produce byte-identical
  `ChainCommitment` bytes) cannot be completed while this stands: the authenticated half never
  reaches the commitment write.
- [`incremental-draft-witness.md`](../proposals/incremental-draft-witness.md) (implemented) and
  [`draft-provenance.md`](../proposals/draft-provenance.md) (proposed) — the draft machinery this
  sits in. Neither is about the *program output* selection replay, which is where it fires.

## What happens

`cargo raster chain run examples/chain-example/Raster.toml`, authenticated (no `--no-auth`),
completes stages 1 and 2 and panics in stage 3:

```
▸ stage 3/3  report   (phase3-report)
[output] phase3 report → AuthRef { storage: "storage", coordinates: "4294967295/1",
         commitment_len: 32, stored_bytes_len: 305, value: Report { … } }

thread 'main' panicked at crates/raster-runtime/src/tracing/recorder.rs:585:29:
Failed to replay program output selection:
  Missing storage object at coordinates CfsCoordinates([4294967295, 1])
```

The program itself produces the right value — the `Report` is printed in full, and the same stage
under `--no-auth` writes a correct `output.bin`. What fails is the recorder's **independent
re-derivation** of the output selection (`recorder.rs:573-585`), which deliberately resolves the
output against its own storage replica rather than trusting the user process:

```rust
// Independently re-derive the output selection from our own
// storage replica, so the recorded output commitment reflects
// committed storage rather than a claim from the user process.
let witness = self.storage.selection_witness(&reference, …)
    .unwrap_or_else(|error| panic!("Failed to replay program output selection: {error}"));
```

The replica has no object at `[4294967295, 1]` — `[u32::MAX, 1]`, the coordinate a finalized
`Draft` is stored under. Stages 1 and 2 return plain values at ordinary coordinates (`0`, `1`)
and replay fine; stage 3 returns a `Draft<Report>`, and it does not.

## Reproduction

```console
$ cargo build --release -p raster-cli
$ mkdir /tmp/scratch && cd /tmp/scratch
$ .../target/release/cargo-raster raster chain run \
    .../examples/chain-example/Raster.toml
```

Stage 3 panics as above. With `--no-auth` the same three stages complete and write a
`ChainCommitment`.

**Not a regression.** Confirmed by stashing all working-tree changes and rebuilding at `HEAD`
(`359582f`): the baseline panics identically, same coordinates, same line. The
`chain-io-commitment` work neither caused nor touches it — that change is in `raster-core`,
the transition guest, `raster-prover::trace` and `raster-cli::chain`, while this is in
`raster-runtime`'s recorder.

## Why it was invisible

Three things had to line up, and they did:

1. `chain_stage_cli.rs` only ever invokes `chain run --no-auth` (`Fixture::chain`, `:52-61`).
2. `examples/chain-example` is the only multi-stage fixture in the tree, and it was authored by
   `chain-stage-execution.md` specifically to exercise the *unauthenticated* per-stage loop.
3. `phase3-report` is the one stage that returns a `Draft` — chosen, per that proposal's
   §Verification, because assembling a report "one line per tile call through a `Draft<Report>`"
   is *"the shape the authoring rules exist to prevent"*. The fixture reached for the hardest
   shape and then never ran it in the posture that checks it.

`examples/hello-tiles` returns a `String`, not a draft, so the single-program authenticated path
does not hit this either.

## What this is not

- **Not the `--no-auth` path.** That mode installs no trace publisher
  (`unauthenticated-execution.md` §6), so the recorder never runs and the replay never happens.
- **Not a soundness hole.** It fails closed: the run aborts rather than recording an output
  commitment the replica cannot substantiate. The check is doing exactly what its comment says.
- **Not `window-seed-reconstruction`.** That is a *guest*-side mid-loop window rejection. This is
  host-side, before any proving, and fires on a program with no recur window involved.

## Directions

Sketched, not chosen.

- **Populate the replica with finalized drafts.** If the finalize path writes the draft's object
  into the recorder's replica under the same `[u32::MAX, n]` coordinate the runtime reports, the
  existing lookup succeeds unchanged. Cheapest if the object is already materialized somewhere at
  finalize time; needs checking against `incremental-draft-witness`'s frontier, which deliberately
  avoids holding whole values.
- **Teach `selection_witness` the draft coordinate space.** Resolve `[u32::MAX, n]` through the
  draft tracker rather than the storage log. Keeps the replica untouched but adds a second
  addressing path to a function whose whole point is that there is one.
- **Make a draft output ineligible as a program output.** Force `finalize` into ordinary storage
  before `ProgramEnd`. Simplest to verify, and the most likely to be wrong — it would forbid the
  shape `chain-example` was written to demonstrate.

## Cost of leaving it

No authenticated multi-stage chain can complete if any stage returns a draft, so: no
`chain-commitment` from an authenticated run of such a chain, no `commit.bin` for its last stage,
and therefore no dispute over that stage. It also blocks `chain-io-commitment`'s V4 equivalence
test, which is the check that the cheap and authenticated postures agree.
