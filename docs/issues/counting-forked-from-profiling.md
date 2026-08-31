# Issue: `counting-forked-from-profiling` — two accumulators answer "what did this run execute"

Status: open 2026-08-31. Unowned.

Reproduced against the working tree of `feature/recur-defered-finilize` at `7c938fd`.
`crates/raster-runtime/src/tile_census.rs` is **untracked**; line numbers below are that file as
it stands, and the `tracing.rs` numbers are the modified file, not `7c938fd`'s. Every other
citation is committed code.

Related:
- [`unauthenticated-execution.md`](../proposals/unauthenticated-execution.md) §6.2 *"Profiling is
  refused, not warned"* — **the decision this forked around, and it does not cover the fork.**
  It rules that a profile of an unauthenticated run measures a different program and must abort
  rather than warn. It rules on *timings*. It says nothing about counts, and nothing about what a
  `--no-auth` operator should read instead — which is the hole `tile_census` was dropped into
  without a design.
- [`artifact-inspection.md`](../proposals/artifact-inspection.md) — proposed 2026-08-21, owns
  *reading a raster artifact back* (`cargo raster show`). Its scope is the value artifacts a
  program produces, not the diagnostic artifacts a run emits beside them. A second untyped JSON
  in the run dir is a thing it would eventually have to know about; today it does not.
- [`trace-event-vocabulary.md`](../proposals/trace-event-vocabulary.md) — implemented 2026-08-13,
  owns the `TraceEvent` variant names. §3 put the vocabulary table in the enum's doc comment.
  `trace_event_variant` (`tracing.rs:232`) is now a **second, hand-maintained copy** of that
  vocabulary as strings. That proposal did not anticipate a second copy and does not own keeping
  them in step.
- [`chain-stage-execution.md`](../proposals/chain-stage-execution.md) — owns the per-stage output
  directory the census keys off (`RASTER_OUTPUT_DIR`).

## 1. What exists, mechanically

Two independent accumulators now count the same run, and they agree on nothing — not their
gating, not their lifetime, not their output format.

**`profiling`** (`crates/raster-runtime/src/profiling.rs`) — a non-default cargo feature
(`raster-runtime/Cargo.toml:23-26`), compiled to no-op stubs when off (`:362`, `:380`, `:400`,
`:420`, `:476`, `:532`). `record_tile_profile` (`:480`) emits a `TileProfileRecord` per tile
invocation carrying `invocation_index`, `tile_id`, `depth`, `coordinates`, three timings and
`input_bytes`/`output_bytes`. The container is versioned and identified: `ExecutionProfile
{ version: 3, run_id, program_total_duration_ns, records }` (`:24-44`), serde-serialized to
`profile.json` (`raster-cli/src/commands.rs:52`) and read back by `cargo raster analyze` (`:228`).
It is **refused outright** when the run is unauthenticated: `reject_profiling_without_authentication`
panics if any of the three `RASTER_PROFILE_*` vars is set (`tracing.rs:142-166`), reading the env
directly "so the refusal holds whether or not the `profiling` feature is on". The CLI cooperates
by never setting those vars for a `--no-auth` run (`raster-cli/src/runtime_env.rs:136-147`, with
the assertion at `:187-192`).

**`tile_census`** (`crates/raster-runtime/src/tile_census.rs`) — no feature gate, env-gated
(`RASTER_TILE_CENSUS_PATH`, or `RASTER_TILE_CENSUS=1` plus `RASTER_OUTPUT_DIR`, `:21-34`), two
global `Mutex<BTreeMap>`s (`:36-44`), hooked from `publish_trace_event`: tiles at `tracing.rs:210`
(before the recur-depth rewrite), event variants at `:224` (after it). Written from
`tracing::finish` (`:195`) as a hand-built JSON string (`tile_census.rs:77-101`).

## 2. The gap: nothing owns counting

`profiling`'s refusal is correct *for timings* and is argued well. But `record_tile_profile`
carries `tile_id`, `coordinates`, `invocation_index` and `input_bytes` alongside those timings,
and none of those four measure work that `--no-auth` deletes. A tile ran or it did not; its
coordinate is the CFS's, not the runner's; `input_bytes` is a serialization width, which
`--no-auth` may change but does not erase.

So the refusal is whole-artifact, and the thing it takes away is not whole-artifact. `tile_census`
is what that shortfall produced: rather than the mode-independent fields becoming available,
a parallel module re-derives a strict subset of them from a different hook, with different gating,
into a different file, and the two can never be joined — `ExecutionProfile` carries `run_id`
(`profiling.rs:27`, `PROFILE_RUN_ID_ENV` at `:22`) and the census carries no identifier at all.

**The operator-facing consequence.** There is no single question "what did this run execute" — there
are two, and which one is answerable depends on a flag the asker may not have chosen. Under
`--no-auth`, `RASTER_PROFILE_PATH` aborts the run; the census must be asked for by a different
name, and produces less. Under authentication both work, overlap partially, and disagree in shape.

### The claim in the census's own header does not hold for half of it

`tile_census.rs:1-7` opens: *"A count of tile executions, available in **both** authentication
modes … a tile either ran or it did not."* True for the `tiles` map. **False for the
`trace_events` map**, which is in the same file, the same env var and the same artifact.

Four `TraceEvent` variants are published only when authenticated. The recur drivers early-return
before their `Start`/`End` publishes — `recur.rs:733` (the new `finalize = false` wrapper), `:856`
(the closed wrapper), `:1251` (recur sequence) — so `RecurTileStart`, `RecurTileEnd`,
`RecurSequenceStart` and `RecurSequenceEnd` are **structurally zero under `--no-auth`**, and
nothing in the artifact says so. `ProgramStart` (`entrypoint.rs:48`, `:63`), `SequenceStart`/`End`
(`lib.rs:2740`, `:2797`, `:2849`) and `TileExec` (`:2535`, `:2611`) are gated only on `cfg(std)`
and do appear in both modes.

An artifact whose header promises mode-independence and whose second map silently loses four
variants in one mode is worse than either half alone: a `--no-auth` census that reads
`"RecurTileEnd": 0` is indistinguishable from a program that ran no recur.

The `trace_events` map is also justified on a different axis from the `tiles` map — `:57-59` ties
it to `TraceCommitment` fingerprint sizing, "one `bits_per_item` slot each". That is an
*authenticated-only* concern; §6 of `unauthenticated-execution` establishes there is no trace and
therefore no trace commitment. So the two maps are fused into one artifact despite one of them
having no meaning in the mode the artifact exists to serve.

## 3. The artifact is untyped and partly redundant

**Hand-built JSON.** `finish` (`:77-101`) concatenates the document with `String::push_str`,
including the two map loops, which differ only in their key type. `profiling` reaches for serde
for the same job. No writer-side escaping: every key today is either a Rust identifier (tile
names come from `stringify!` of the tile ident — `lib.rs:2525`, `recur.rs:372`) or a literal from
the closed match in `trace_event_variant`, so no unescapable key is currently reachable. The
writer does not encode that constraint, and nothing fails if it stops holding.

**No `version`.** `ExecutionProfile` carries one and is on `3` (`profiling.rs:38`), i.e. the
format has already moved twice. A census consumer has nothing to branch on.

**No `run_id`.** So a census cannot be joined to the profile of the same run — the exact operation
§2's fork creates a need for.

**`total_tile_executions` is derived, not measured.** `note_tile_execution` fires on
`TraceEvent::TileExec` *before* the recur-depth rewrite (`tracing.rs:206-210`); `note_trace_event`
fires *after* it (`:224`), so a depth>0 `TileExec` is recorded as `RecurTileIterationExec`. Every
tile execution is therefore counted exactly twice, and:

```
total_tile_executions  ==  trace_events.TileExec + trace_events.RecurTileIterationExec
```

is an identity, not a cross-check — it holds by construction and can never disagree. Three of the
artifact's four top-level fields are functions of the other one.

## 4. What this is not

- **Not a soundness issue.** Nothing here is read by a guest, enters a commitment, or affects
  what a run computes. It is a diagnostics-surface issue.
- **Not an argument against §6.2's refusal.** The refusal of *timings* is right, and this issue
  does not reopen it. The claim is narrower: the refusal is applied at whole-artifact granularity
  to an artifact that is not homogeneous.
- **Not the gating question.** `RASTER_TILE_CENSUS=1` with no `RASTER_OUTPUT_DIR` disables
  counting silently (`:27-31`) where the profiling vars panic (`tracing.rs:159`), and the mutex
  pair is taken per event inside the hot recur sweep (`:51`, `:64`). Both are real; both are
  downstream of whichever direction §5 takes, and neither is worth designing against on its own.
- **Not `artifact-inspection`'s.** That proposal reads value artifacts back. Whether it should
  also read diagnostic ones is a question for it, after this is settled.

## 5. Directions, none chosen

1. **Split `profiling` along the mode-independence line** — a counting half always available, a
   timing half still refused. Answers: §6.2's refusal is deliberately coarse and env-level so it
   holds "whether or not the `profiling` feature is on" (`tracing.rs:149-151`); making it
   per-field means a `profile.json` that is present but partly empty under `--no-auth`, which is
   exactly the "invites the comparison" failure the refusal was written to prevent. It would need
   a name and a shape that cannot be mistaken for a profile.
2. **Keep a separate artifact, but type it** — serde struct, `version`, `run_id`, drop the
   derived total, and either drop `trace_events` or mark the variants that are authenticated-only.
   Answers: leaves two artifacts, two env conventions and two things an operator must know to ask
   for, and does not say which one `cargo raster analyze` grows to read.
3. **Derive counts from the trace instead of accumulating at run time.** Answers: an
   unauthenticated run emits no trace at all (`unauthenticated-execution` §6), which is the mode
   the census exists for — so this is dead for the motivating case. It may still be right for the
   authenticated half, and if so the authenticated half needs no accumulator.
4. **Make it a trace *format* rather than an artifact** — a counting `Publisher` selected through
   `RASTER_TRACE_FORMAT`, reusing the existing publisher plumbing instead of a parallel hook.
   Answers: `init` installs no publisher when unauthenticated (`tracing.rs:89-96`), and doing so
   would contradict `init_with`'s stated reason for staying authenticated (`:131-134`).

Directions 1 and 4 both require deciding what an unauthenticated run is *allowed* to report, which
is the question §6.2 answered for timings and left open for everything else. That decision is
upstream of all four and should probably be made first.

## 6. Reproducing

```bash
# the two accumulators, side by side
sed -n '1,34p;46,66p;69,108p' crates/raster-runtime/src/tile_census.rs
sed -n '20,44p;480,487p' crates/raster-runtime/src/profiling.rs
grep -n 'cfg(not(feature = "profiling"))' crates/raster-runtime/src/profiling.rs   # the no-op stubs
sed -n '23,26p' crates/raster-runtime/Cargo.toml                                   # not a default feature

# the refusal this forked around, and the CLI honouring it
sed -n '142,166p' crates/raster-runtime/src/tracing.rs
sed -n '136,147p;185,193p' crates/raster-cli/src/runtime_env.rs

# §3's identity: the tile hook fires before the depth rewrite, the event hook after
sed -n '205,229p' crates/raster-runtime/src/tracing.rs

# §2's four authenticated-only variants — each early-returns before its Start/End publish
grep -n 'is_authenticated' crates/raster-macros/src/recur.rs        # 733, 856, 1251
grep -n 'RecurTileStart\|RecurSequenceStart' crates/raster-macros/src/recur.rs
# ... and the ones that are not gated, for contrast
grep -n 'TraceEvent::TileExec\|TraceEvent::SequenceStart' crates/raster-macros/src/lib.rs

# the second copy of the event vocabulary
sed -n '232,247p' crates/raster-runtime/src/tracing.rs
grep -n -A20 'enum TraceEvent' crates/raster-core/src/trace.rs
```

Producing both artifacts for the same program, which is what makes the overlap concrete:

```bash
# authenticated: profile + census, no shared identifier between them
RASTER_TILE_CENSUS=1 cargo raster run --input input.json --input-manifest input_manifest.json
cat target/raster/runs/<run_id>/tile_census.json
cat target/raster/runs/<run_id>/profile.json | head -20

# unauthenticated: the profile var aborts the run; the census is all there is,
# and its four recur variants read 0 whatever the program did
RASTER_PROFILE_PATH=/tmp/p.json cargo raster run --no-auth --input input.json   # panics
RASTER_TILE_CENSUS=1           cargo raster run --no-auth --input input.json
```

A program with a `call_recur!` is required for the last line to show the loss — `examples/` has
one; `raster-chain-inference` stage `prefill-range` is the realistic case.
