# Issue: `authenticated-chain-draft-output` — a finalized draft has no object in the recorder's storage replica

Status: open 2026-08-27. Unowned. **Deterministic reproducer: `--fraud-step 22` on `examples/hello-tiles` (§Second reproducer).** **Scope widened 2026-09-18: not chain-specific, and not
specific to program outputs.** The name is kept so links stay valid; read it as *draft-object
missing from the replica*. A second reproducer on `examples/hello-tiles` fails on the
**fraud-proof** path, where the draft is a tile *input*, not a stage output.

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

> **Still reproduces after the 1-based signed-coordinate change (2026-09-18), with the draft
> sentinel renumbered.** `CfsCoordinate` is now `i32` and `DRAFT_NAMESPACE` moved from `u32::MAX`
> to `i32::MIN`, so the coordinate below now reads `[-2147483648, n]`. Confirmed on
> `examples/chain-example` with every stage's tile guests rebuilt from scratch: stages 1 and 2
> commit cleanly, stage 3 panics at `recorder.rs:642` with `Missing storage object at coordinates
> CfsCoordinates([-2147483648, 2])`. Same defect, same line, new spelling — read `u32::MAX` as
> `DRAFT_NAMESPACE` throughout the rest of this file.

## Second reproducer: the fraud path, on a program that is not a chain

**Deterministic as of 2026-09-21** — `--fraud-step` (`run.rs`, `FraudTarget`) replaced the
injector's unseeded random choice, so this is now a one-line reproducer rather than a draw from
58 eligible steps:

```console
$ cd examples/hello-tiles
$ cargo raster run --input input.json --input-manifest input_manifest.json \
    --commit probe22.bin --fraud-proof-window-size 32 --fraud-step 22
  Fraud injected into 1 of 58 eligible steps:
    index       22
    exec_index  40
    target      tile concat_messages
    coordinates CfsCoordinates([9])
$ cargo raster run --input input.json --input-manifest input_manifest.json --audit probe22.bin
```

```
run.rs:966 → Failed to build storage selection witness for 'message1':
             Missing storage object at coordinates CfsCoordinates([-2147483648, 2])
```

Exit `101`: fraud detected, receipt cannot be built. **It fails in seconds** — the witness is
built host-side in `prove()` before any proving starts, so this reproducer is cheap to re-run.
Contrast [`sequence-scope-forbids-narrowing`](./sequence-scope-forbids-narrowing.md)
(`--fraud-step 45`), whose panic is inside the guest and therefore arrives only after ~2.5 h of
proving.

> **The "in seconds" claim is stale as of 2026-09-24, on a tree carrying uncommitted `run.rs` work
> (`7e2ce27` "witness selection for parent" and later).** The injection still reproduces exactly —
> index 22, `exec_index` 40, tile `concat_messages`, `CfsCoordinates([9])` — but the audit no longer
> stops at the witness build: it ran the program to completion, printed its output artifacts, and
> went into proving, still running after 15 minutes at ~16 cores. So this reproducer is **no longer
> cheap**, and whether the underlying defect is fixed on that path or merely moved is **unresolved**
> — the run was killed rather than finished. Anything citing this as a fast check needs re-basing;
> [`storage-role-split`](../proposals/storage-role-split.md) §Verification did, and says so.

`--fraud-step 22` is the index in that listing; `exec:40` names the same step by `exec_index`.
`--fraud-step list` prints the table. The flag enables the injector on its own, so the commitment
file needs no `fraud_` prefix.

Same missing object, different caller. `message1` is `concat_messages`'s first parameter
(`src/lib.rs:123`), bound in `main` to `select!(String, draft_greeting.clone().title)` after
`finalize(draft)` (`src/main.rs:101-105`) — a selection **into** a finalized draft, consumed as a
tile input. Nothing here is a chain, and nothing here is a program output.

So the defect is one fact reached from two directions, and both callers are simply asking storage
for an object at a draft coordinate:

| caller | what it wants | program |
| --- | --- | --- |
| `recorder.rs:642` — program output selection replay | the draft **as the program's output** | `chain-example` stage 3 |
| `run.rs:765` — `build_storage_selection_witnesses` | the draft **as a tile's input source** | `hello-tiles` fraud proof |

The honest path is unaffected in both: `hello-tiles` commits and audits clean
(`Verification Success`), and detection works — the audit exits `101`. It is *proof construction*
that cannot proceed, which is this issue's whole shape.

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

Sketched, not chosen. The second reproducer narrows the field: any fix has to serve **both**
callers in the table above, and one of the three original sketches no longer can.

- **Populate the replica with finalized drafts.** If the finalize path writes the draft's object
  into the recorder's replica under the same `[DRAFT_NAMESPACE, n]` coordinate the runtime
  reports, both lookups succeed unchanged — neither caller needs to know a draft was involved.
  Cheapest if the object is already materialized somewhere at finalize time; needs checking
  against `incremental-draft-witness`'s frontier, which deliberately avoids holding whole values.
  **Strengthened** by the second reproducer: it is the only sketch that fixes an input binding and
  an output selection with one change, because it fixes the thing both of them ask.
- **Teach `selection_witness` the draft coordinate space.** Resolve `[DRAFT_NAMESPACE, n]` through
  the draft tracker rather than the storage log. Also serves both callers, since both reach
  storage through that function. Keeps the replica untouched but adds a second addressing path to
  a function whose whole point is that there is one — and the fraud-proof guest would then have to
  learn the same second path, or the witness it is handed proves membership in a structure the
  guest cannot check.
- ~~**Make a draft output ineligible as a program output.**~~ **Ruled out 2026-09-18.** It would
  not touch `hello-tiles`, where the draft is a tile *input* (`message1`) and never the program's
  output. It only ever addressed the chain reproducer, and the defect is not about outputs.

The second sketch carries a consequence worth stating before anyone picks it: the fraud path does
not merely *read* the object, it builds a witness the transition guest verifies against the
authenticated store's roots. A resolution route that exists only host-side produces evidence the
guest has no way to accept. The first sketch does not have this problem — a draft written into the
replica is an ordinary storage object with ordinary membership, which is what every existing check
already knows how to verify.

## Cost of leaving it

No authenticated multi-stage chain can complete if any stage returns a draft, so: no
`chain-commitment` from an authenticated run of such a chain, no `commit.bin` for its last stage,
and therefore no dispute over that stage. It also blocks `chain-io-commitment`'s V4 equivalence
test, which is the check that the cheap and authenticated postures agree.

**Raised 2026-09-18.** With the second reproducer, this also blocks **fraud proof construction for
any program that selects into a finalized draft** — chain or not. On `hello-tiles` the honest path
is entirely healthy (commit `0`, audit `Verification Success`) and fraud is *detected* (`101`), but
the receipt cannot be built. That is a completeness defect on the dispute path, and on the dispute
path a completeness defect is a lost dispute — the same framing
[`fraud-evidence-storage-unavailable`](./fraud-evidence-storage-unavailable.md) §3 uses. As of that
date it looked like the **last** blocker standing between a detected divergence and a completed
fraud proof on `hello-tiles`.

> **Retracted 2026-09-21.** It is not. A fraud probe whose injector drew a different random victim
> step reached a different wall — [`sequence-scope-forbids-narrowing`](./sequence-scope-forbids-narrowing.md),
> a guest assertion at `checks/cfs.rs:59` that forbids a sub-sequence from `select!`ing into its own
> parameter. The original claim rested on a run that happened to draw a draft-dependent step; the
> injector corrupts a **randomly chosen** step (`run.rs:537-545`), so no single probe establishes
> what the next one hits. The honest count of remaining blockers on the fraud path is **unknown**.
> At ~2.5 h per probe, a targetable injector is the cheapest way to make it knowable.
