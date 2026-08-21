# Proposal: `chain-stage-execution` — per-stage re-execution in an unattested chain run

Status: **partly implemented** (2026-08-21) — §2 (`latest`, `--run`), §3 (`--stage`,
rehydration, the moved spec-validity check) and §4 (invalidation) have landed on the *existing*
`--no-auth` surface: `cargo raster chain run --no-auth --stage <name> [--run <dir>]`, writing to
`target/raster/chains-no-auth/`. Verified end-to-end on a real three-stage chain
(`crates/raster-cli/tests/chain_stage_cli.rs`), for which this proposal also supplies the missing
chain fixture `examples/chain-example` — see §Verification. **§1 has not landed.**
Promoting the mode from a flag to a command, and the `chains-dry/` rename, are blocked on the
naming question in §Open questions — which is not a preference but a conflict with a recorded
decision (see below). The feature itself is independent of the name, so it was built first.
Companion to: [`program-chain.md`](./program-chain.md) (partly implemented),
[`unauthenticated-execution.md`](./unauthenticated-execution.md) (implemented)
Depends on: `unauthenticated-execution.md` §6.1 — a cheap stage still produces a real
`output.bin`, which is the entire mechanism this proposal leans on.

## Problem

`chain::run()` (`crates/raster-cli/src/chain.rs:128`) is one loop over `spec.stages`, all or
nothing. A 74-stage pipeline like `raster-inference` re-executes all 74 stages to see the effect
of a change to stage 40.

That is correct and unavoidable for an authenticated run — a `ChainCommitment` is a statement
about a whole chain, and there is no coherent partial one. But it is also what the *development*
loop does, and there the cost has no counterpart in value: `chain run --no-auth` attests nothing,
writes no `commit.bin`, writes no chain-commitment (`chain.rs:302-311`), and still pays for every
upstream stage on every iteration.

The artifacts a mid-chain stage needs are already on disk. After a `--no-auth` run, every stage
directory holds `input.json`, `input_manifest.json`, `output.bin`, `output.rindex`, and
`output_manifest.json`. Nothing is missing. What is missing is a way to say "run only this one,
against what is already there."

The obstacle is not data, it is *where the data lives in the program*: the three values that cross
stage boundaries — `output_commitments`, `stage_index`, `checkpoints` (`chain.rs:176-186`) — are
loop locals, accumulated as stages complete. A stage in the middle has no way to obtain them
except by running everything before it.

## Goal

`cargo raster chain dry-run --stage <name>` — execute exactly one stage of a chain, in place,
against an existing run directory, in the unattested mode.

Non-goals, stated up front:

- **Not `--from <stage>`, and not a resume driver.** One stage per invocation. Deciding what to
  re-run is the developer's job, and a range flag is a scheduler in disguise.
- **Not per-stage *commitment*.** This proposal never touches the authenticated path. `chain run`
  keeps today's all-stages-or-nothing behaviour, unchanged. See §5.
- **Not mixed-posture chains.** `unauthenticated-execution.md` §10 names that open policy
  question — "what a chain commitment means when its stages were run at different postures" —
  and this proposal does not approach it. It is the *other* half of §10: the cheap execution
  primitive, with no commitment story attached because there is no commitment.

## Facts the design builds on (verified in code)

- **`collect_output` (`chain.rs:1121`) is already called in the unattested mode.** It sits under
  `if produces_output` (`chain.rs:267`), not under an authentication branch. It recomputes both
  link hashes from `output.bin` and cross-checks the structural root against `output_manifest.json`
  (`chain.rs:1136`). Rehydrating a producer's commitment is therefore not a new mechanism — it is
  an existing call, made earlier.
- **A stage's execution is a pure function of `(program, input.json, input_manifest.json)`.**
  `build_and_run_stage` (`chain.rs:885`) passes exactly those, and `synthesize_inputs`
  (`chain.rs:1006`) derives the latter two from the producers' artifact bytes and paths. This is
  why running a stage in isolation is the same operation as running it in sequence.
- **`stage_index` currently doubles as "has this stage run?"** It is populated at `chain.rs:281`,
  after a stage completes, and read at `chain.rs:1034` to resolve a `from` binding. The separate
  "produced no output" case is already handled one line later against the commitment's emptiness
  (`chain.rs:1042`), so the two roles are already distinguishable.
- **The unattested mode already skips program identity entirely** (`chain.rs:141`, and the
  reasoning in `unauthenticated-execution.md` §10): the pre-run fail-fast is not run, because
  identity is read only for checkpoints, and requiring it would break the mode in exactly the case
  it exists for — a source change whose `Raster.lock` has not been rebuilt. Re-running a stage
  whose source just changed is therefore already coherent in this mode and needs no new rule.
- **`chain_run_id()` (`chain.rs:1413`) mints a fresh `<nanos>-pid<pid>` directory per invocation.**
  Every run lands somewhere new, so a per-stage invocation has nothing to point at without a way
  to name an existing directory.
- **Unattested runs live under their own root** (`no_auth_chains_root`, `chain.rs:1402`), so
  `latest_chain_commitment()`'s newest-dir-wins discovery (`chain.rs:1343`) can never land on a
  directory with no commitment in it. That separation is load-bearing and this proposal preserves
  it.
- **`RuntimeEnv` already models the posture as two types**, with the authenticated form reachable
  only through `authenticated()` (`crates/raster-cli/src/runtime_env.rs:33`), so a trace cannot be
  set on a run that would refuse one. The CLI is the one layer where the posture is still a
  boolean flag.
- **Single-program runs share one root.** `run --no-auth` writes to `target/raster/runs/<id>/`
  either way (`crates/raster-cli/src/commands.rs:49`); nothing discovers over that directory, so
  it needs no split.
- **`--no-auth` has never shipped.** Both flags (`main.rs:185`, `main.rs:229`) are on
  `feature/native-run` only (`63ecda6`), not on `main`.

## Design

### 1. `dry-run` becomes a command, not a flag

```
cargo raster dry-run                     # single program   (was: run --no-auth)
cargo raster chain dry-run               # all stages       (was: chain run --no-auth)
cargo raster chain dry-run --stage embed # one stage
```

Both `--no-auth` flags are **removed**, not deprecated — there is nothing to keep compatibility
with.

The gain is more than naming. `run --no-auth` needs `conflicts_with_all = ["commit", "audit"]`
(`main.rs:185`) to stay coherent; a separate command has no `--commit`/`--audit` to conflict with.
That is the same "make the invalid state unrepresentable" move `RuntimeEnv` already makes for the
trace, applied one level up, and it is what makes `--stage` safe to add: there is no flag
combination that could ask for a per-stage *commitment*.

**The spelling is unresolved, and this section has not been implemented.** `dry-run` reverses a
recorded decision: `unauthenticated-execution.md` §Naming chose `--no-auth` over `Plain` /
`Direct` / `Lightweight` because a name should say "what is absent rather than how the run feels",
and that document's Related list already reserves `--dry-run` for `zkvm-dry-run.md`. `dry-run`
loses on both counts — it describes the feel, and it takes a reserved term. The candidates are in
§Open questions. Whichever wins, the shape above is unchanged: a command, not a flag, with the
directory root named to match.

### 2. The run directory, and `latest`

```
target/raster/chains-dry/          (renamed from chains-no-auth/)
    00017…-pid4021/
    00018…-pid5566/
        prompt_prepare/  embed/  decode_0/  …
    latest -> 00018…-pid5566
```

`latest` is a symlink, updated **only when a directory is minted** — that is, by a full
`chain dry-run`. A `--stage` invocation runs *inside* an existing directory and never repoints it;
otherwise `latest` would come to mean "most recently touched", and a `--stage` run against an
older directory would silently drag the pointer backwards.

Where symlink creation fails, fall back to a plain file named `latest` holding the directory name.
Reading accepts either.

Run-directory resolution, in order:

1. `--run <path>`, explicit;
2. `latest`;
3. error, naming the full `chain dry-run` that would create one.

So the common case is `cargo raster chain dry-run --stage embed`, with no path to look up. `--run`
survives for the "not the latest one" case.

The authenticated `chains/` root gets no `latest` — `latest_chain_commitment()` already does
newest-dir discovery there, and nothing per-stage points at it.

### 3. `--stage`: rehydrating the one thing a mid-chain stage needs

For each `from` binding of the named stage, resolve the producer's directory and call the existing
`collect_output` on it. That yields the structural commitment `synthesize_inputs` needs, recomputed
from `output.bin` and cross-checked against `output_manifest.json` — the same value, by the same
code, that the producing stage's own run computed.

Two changes make it fit:

- **`stage_index` is built from the spec up front**, for all stages, rather than accumulated. The
  "has this stage run?" role moves entirely onto the resolved commitment being absent.
- **`outputs` becomes sparse** — `Vec<Option<Vec<u8>>>` sized to the spec, filled only for the
  producers the named stage actually binds. A whole-chain run fills it left to right as today.

The missing-producer error is the feature's main surface, so it names the fix:

```
stage 'decode_0': parameter 'kv_cache' is fed from 'embed', which has no output.bin
  expected: target/raster/chains-dry/00018…-pid5566/embed/output.bin
  run: cargo raster chain dry-run --stage embed
```

One check moves while this is being touched: the "producer must appear earlier in the spec" test
currently lives only in `audit()` (`chain.rs:527`). It is a *spec-validity* check, not a
verification one — it asks whether the manifest is well-formed — and it belongs beside
`validate_stage_names` (`chain.rs:1369`), running before anything executes, in every mode. With
`--stage` it stops being optional: a single-stage invocation can name a producer that appears
later, which a sequential run could never reach.

### 4. Downstream invalidation

Re-running stage *k* deletes the stage directories of *k+1..N* before running.

**"Following" means spec order, not dependency closure.** In a linear v1 chain the two coincide.
Where they diverge — a downstream stage bound only to `external` inputs — spec order over-deletes
rather than under-deletes. Over-deleting costs recompute; under-deleting leaves a stale artifact
that looks fresh and silently feeds a later comparison. Take the cheap failure.

**The whole stage directory goes, not just the outputs.** A downstream `input.json` /
`input_manifest.json` was synthesized against the *old* upstream commitment (`chain.rs:1068-1073`)
and is stale in exactly the same way. Leaving it behind is how a stage would later re-run against
a manifest committing a value that nothing produces.

It is announced, because it can be expensive:

```
▸ stage embed  (re-run in place)
  invalidating 71 downstream stages: decode_0 … decode_34, sample, detokenize
```

No `--keep-downstream` escape hatch. The directory is `target/` scratch, and a developer who
wanted the downstream results did not want to re-run the upstream stage.

### 5. What is deliberately absent

Everything the authenticated path needs, because the authenticated path is untouched:

| | mints dir | moves `latest` | trace | `commit.bin` | chain-commitment |
| --- | --- | --- | --- | --- | --- |
| `chain run` | ✓ `chains/` | — | ✓ | ✓ | ✓ |
| `chain dry-run` | ✓ `chains-dry/` | ✓ | — | — | — |
| `chain dry-run --stage` | — | — | — | — | — |

No `StageCheckpoint` is written per stage, no assembly step, no staleness rule at the commitment
level, and no claim that a stage run alone yields the same checkpoint as a stage run in sequence.
Those were all consequences of putting per-stage execution in commitment mode, and none of them
arise here. `raster-core`, `raster-prover`, the guests, `chain audit`, and `chain fraud-prove` are
unchanged.

An earlier draft of this proposal did put it in commitment mode, reasoning backwards from
`unauthenticated-execution.md` §10's "enter commitment mode only for the stage whose output is
contested". Every hard part of that draft existed to answer "what does a chain commitment mean
when its stages were committed at different times." Entering from the unattested side means the
question is not asked.

## Alternatives considered

- **`--from <stage>` (that stage and everything after).** Rejected: it is the scheduling decision,
  taken away from the developer, and once the range exists the natural next request is dependency-
  aware selection — which is a build system, not a chain runner. One stage per invocation composes
  into the same thing under shell control, and manual resume after a mid-chain failure still works
  one stage at a time.
- **Per-stage `checkpoint.bin` files, assembled into `chain-commitment`.** This is what per-stage
  execution in the *authenticated* path would need, and it has a real independent benefit: a
  74-stage chain that dies at stage 7 currently discards seven stages of proving work. Deferred
  because it is inseparable from the §10 policy question, and because it is a chain-commitment
  format concern rather than a dev-loop one.
- **Auto-invalidation by dependency graph** instead of spec order. Rejected for v1 as more
  machinery than a linear chain can justify; revisit with DAG chains, where the two stop
  coinciding in practice rather than in principle.
- **Warning instead of deleting downstream stages.** Rejected: a stale `output.bin` that is
  merely *announced* as stale is still the thing the next command reads.
- **Splitting `target/raster/runs/` into a `runs-dry/` sibling**, for symmetry with the chain root.
  Rejected: the chain split exists for a mechanical reason (newest-dir discovery must not land on a
  commitment-less run) that has no analogue for single programs, where nothing discovers over the
  directory. Copying the shape without the reason is how a convention becomes noise.

## Implementation plan

1. **Rename** `chains-no-auth/` → `chains-dry/` and `no_auth_chains_root` → `dry_chains_root`
   (`chain.rs:153`, `:1398-1404`), plus the two documents naming the old path
   (`unauthenticated-execution.md:539`, `README.md:35`).
2. **Promote the mode to a command** — `Commands::DryRun` and `ChainCommand::DryRun`; delete both
   `--no-auth` flags (`main.rs:185`, `:229`) and the `conflicts_with` clauses they needed. The
   run paths already branch on a boolean (`chain.rs:950`, `commands/run.rs:164`), so this is a
   caller change, not a runner change.
3. **`latest`** — write on directory creation, resolve on read, with the text-file fallback.
4. **Spec-validity checks** move beside `validate_stage_names`: producer-appears-earlier
   (from `chain.rs:527`), and a named `--stage` existing in the spec.
5. **Sparse rehydration** — `stage_index` from the spec, `outputs` as `Vec<Option<_>>`, producer
   resolution via `collect_output`, and the missing-producer error of §3.
6. **Downstream invalidation** with the announcement of §4.

Steps 5 and 6 are the feature; 1–4 are what make it expressible.

## Verification

`program-chain.md`'s implementation order step 5 — an end-to-end multi-stage chain — was never
done, and no chain fixture existed in this repo. So this proposal supplies one:
**`examples/chain-example`**, a three-stage chain (`normalize → aggregate → report`) modelled on the
`raster-pipeline` reference. Three stages is the minimum that expresses what per-stage execution
is *for*: a **middle** stage re-run, and invalidation of **more than one** downstream stage. Two
stages can express neither.

It is a real raster program, not a harness: phase 1 filters with an output-building `call_recur!`,
phase 2 folds with a state-only one, and phase 3 assembles its report **one line per tile call**
through a `Draft<Report>` rather than building the whole thing inside a single tile — the shape
the authoring rules exist to prevent.

Landed as `crates/raster-cli/tests/chain_stage_cli.rs` (7 tests), driving the real CLI:

- **Every link holds.** A whole-chain run leaves each stage's `input_manifest.json` commitment
  equal to its producer's `output_manifest.json` commitment, across both links.
- **Equivalence — the claim the feature rests on.** A full run, then `--stage report` alone,
  produces a byte-identical `output.bin`. If rehydration fed the stage anything other than the
  producer's committed bytes, this is where it shows.
- **A middle stage** rehydrates from the stage before it, invalidates exactly `report`, leaves
  `normalize` untouched, and reproduces its own output.
- **The first stage invalidates every later stage**, not merely the next one — announced as
  `invalidating 2 downstream stages: aggregate, report`.
- **Stage-by-stage rebuild converges.** Re-running stage 1, then 2, then 3 lands on the same final
  digest the whole-chain run produced. This is what makes per-stage execution a shortcut rather
  than a second execution semantics.
- **The missing-producer error names the fix**, and **`--stage` needs a prior run** rather than
  minting an empty directory to fail inside.

Unit coverage in `chain.rs` (13 tests) takes the pieces in isolation: `validate_spec` ordering,
the rehydrated-commitment→manifest path, invalidation bounds, `latest` round-trip including the
text fallback, and run-directory resolution.

Not covered: posture isolation (no `--stage` invocation touching `target/raster/chains/`), still
argued structurally from the separate roots rather than tested.

## Open questions

- **What the command is called — the one open decision, blocking §1.** Three candidates:

  | | collides with `zkvm-dry-run` | satisfies §Naming's "name the absence" rule |
  | --- | --- | --- |
  | `dry-run` | yes | no |
  | `exec` | no | no |
  | `unauth` | no | yes |

  The collision is concrete, not stylistic: `zkvm-dry-run.md` §3 defines
  `cargo raster run --dry-run` as "execute every tile in RISC0 without proving" — *more* expensive
  than a normal run, requiring the RISC0 toolchain — against `cargo raster dry-run` for native
  execution with no attestation, *cheaper*, requiring no toolchain. Two opposite meanings one
  space apart, and `zkvm-dry-run` §Open questions item 1 asks whether its flag should become the
  default for plain `cargo raster run`, which would make the pair permanent.

  `dry-run` therefore costs two documents changing their recorded conclusions: this one reverses
  `unauthenticated-execution.md` §Naming, and `zkvm-dry-run.md` §3 must rename its flag
  (`--zkvm-check`, `--price`). `unauth` costs one line — §Naming's "the CLI spelling is
  `--no-auth`" becomes "the CLI spelling is the `unauth` command" — and leaves the reserved term
  reserved. Noted rather than decided because half of it is the other proposal's surface.
- **Should `--stage` accept more than one name?** `--stage a --stage b` is not `--from`, and it
  covers the common "these two changed" case without introducing a range. Left out of v1 because
  invalidation would need to key off the earliest named stage, which is a rule worth having a
  second use case before writing.
- **Should a full `chain dry-run` reuse `latest` instead of minting?** It would make the dev loop
  a single directory and remove the accumulation of run directories entirely. Against: it destroys
  the ability to compare two runs, which is the reason timestamped directories exist.

## Out of scope

- Per-stage execution in the authenticated path, and everything it implies — per-stage
  checkpoints, partial chain-commitments, and the assembly step.
- Mixed-posture chains and on-demand commitment for a contested stage
  (`unauthenticated-execution.md` §10).
- Parallel stage execution. The runner is serial and stays serial.
- Interaction with `chain-repeat.md`: if `[[chain.repeat]]` lands, `--stage` names an *expanded*
  stage, so expansion must happen before stage selection. Noted so whichever lands second inherits
  the constraint; neither blocks the other.
