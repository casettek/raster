# Proposal: `chain-repeat` — repeated chain segments with an authorized trip count

Status: proposed (2026-08-19)
Companion to: [`program-chain.md`](./program-chain.md) (partly implemented),
[`chain-fraud-proof.md`](./chain-fraud-proof.md) (implemented)
Precedent: [`dynamic-index-selection.md`](./dynamic-index-selection.md) (phases 1–3
implemented) — the same "the index must be an authorized value" argument, one level up.

## Problem

A `[chain]` manifest is a flat, hand-written `[[chain.stage]]` list
(`crates/raster-cli/src/chain.rs:59`, `:66`). That is fine when the pipeline's shape is a
property of the *code* — but it breaks down as soon as a segment repeats a number of
times that is a property of the *run*.

`raster-inference` is the forcing case. Its chain is 74 stages: prompt prepare, embed, 35
`prefill_prepare_aux_lN`, 35 `prefill_range_lN`, finalize, and one greedy argmax. That
produces exactly one token. Generating a full response means running a 73-stage decode
step per token — embed, 35 aux, 35 range, finalize, select — with each step's per-layer KV
cache bound to the previous step's same-layer stage.

Today the only way to express that is to write it out:

- **N is a request parameter, not a model property.** 35 layers is fixed forever, so
  unrolling it costs nothing. `max_new_tokens` differs per request, so a 4-token chain
  and a 5-token chain are different manifests. A 20-token run is 1,535 hand-maintained
  stage blocks.
- **The irregularities are where the bugs are.** Step 1's cache comes from prefill;
  later steps' from the previous iteration. Layer 0's activations come from the embed
  stage; later layers' from the previous layer. Every one of those is a `from = "..."`
  string that is correct or silently wrong. `raster-inference` already carries twenty
  such hand-written donor bindings, and nothing cross-checks them (see §7).
- **Generating the manifest from a tool moves the problem without solving it.** The
  repetition stops being visible in the artifact a verifier reads, and the generator
  becomes a second, unversioned source of truth for the pipeline's shape.

What is missing is a way for the manifest to say *this segment repeats, this many times,
and here is the authorized value that says how many* — without the trip count becoming
something the party executing the chain gets to choose.

## Goal

Add `[[chain.repeat]]`: a templated block of stages, expanded before execution, whose
iteration count comes from an authorized value — a manifest literal, a committed external,
or **the authorized output of an earlier stage in the same chain**.

The third case is the one that matters and the one this proposal is written around. It
lets a chain compute its own control flow: a planner stage emits "you will need 7
iterations" as a normal `ProgramEnd` artifact, and the chain expands to 7 iterations,
with a verifier able to check that 7 was the honest answer.

Non-goals, stated up front:

- **Not a `while` loop.** This resolves the count *once*, from a value that exists before
  the repeated block runs. "Iterate until a stage says stop" (EOS in `raster-inference`)
  is a strictly larger feature; §8 sketches it as the natural follow-on and explains why
  it is separable.
- **No new proving.** Expansion is checked by public re-derivation over bytes a verifier
  already holds, in the same spirit as `program-chain.md`'s "chain links are verified by
  public hash equality over artifact bytes, not by a new ZK proof."
- **Linear chains only**, inherited from `program-chain.md` v1.

## Facts the design builds on (verified in code)

- A chain manifest deserializes into `ChainSpec { stages: Vec<StageSpec> }`
  (`crates/raster-cli/src/chain.rs:59`) via `load_spec`, from either a `Raster.toml`
  `[chain]` table or a `chain.json`. Both formats land on the same `StageSpec` /
  `InputBinding` shapes (`:66`, `:78`), so a change made at the `ChainSpec` level is
  format-agnostic.
- `InputBinding` is `External(ExternalRef)` or `From(String)`, where `ExternalRef` carries
  `{ path, index_path, commitment }` — the commitment is a **manifest constant** today,
  never read from file bytes.
- `ChainCommitment { stages: Vec<StageCheckpoint> }` (`crates/raster-core/src/chain.rs`)
  is the verifier-facing object, and `StageCheckpoint` (`:32`) already names
  `program_commitment`, `input_manifest_commitment`, `output_payload_commitment`,
  `output_structural_commitment`, and `trace_commitment_digest` per stage. A verifier
  holding it plus each stage's `output.bin` checks every link with no prover.
- `InputBindingSource::Chained { stage: usize }` (`crates/raster-core/src/chain.rs:21`)
  refers to a producing stage **by index into the expanded list**. Expansion order is
  therefore load-bearing and must be deterministic.
- `chain audit` compares recorded against declared shape by length first —
  `if chain.stages.len() != spec.stages.len()` (`crates/raster-cli/src/chain.rs:371`) —
  then per-stage. This is the check that must not become circular once the length is
  derived rather than declared.
- `ChainFaultKind` is `{ Execution, Link }` (`crates/raster-core/src/chain.rs:72`), with
  `Link` already defined as "an inconsistency inside the `ChainCommitment` itself, proven
  from the manifest the checkpoint committed." A shape fault is the same category of
  object and extends an existing pattern rather than inventing one.
- `program-end.md` established that a stage's `output.bin` + `output_manifest.json` is
  format-compatible with an external input. Reading a scalar out of a stage output is
  therefore the same operation as reading one out of an external — no new resolution path.
- `dynamic-index-selection.md` already settled the analogous question inside a program:
  an index may be a runtime value **provided it is an `AuthRef<uN>`**, because "an index
  with no lineage is one a prover could choose." This proposal applies that rule to a
  trip count.

## Design

### 1. Named chain inputs

Externals are currently spelled inline at every use, so a value referenced twice has its
commitment written twice. Add a `[chain.input]` table so a commitment has one home:

```toml
[chain.input.sampling]
path       = "run/sampling.rastered"
index_path = "run/sampling.rindex"
commitment = "9f2c8ab1…"
```

`inputs.x = { input = "sampling" }` becomes a third `InputBinding` variant resolving to
the same `ExternalRef`. This is independently useful and is a prerequisite for referring
to one value from both a stage binding and a repeat count.

### 2. The repeat block

```toml
[[chain.repeat]]
name  = "decode"
index = "t"                    # bound to 1..=count inside the block
count = { from = "plan_generation", select = "steps", max = 128 }
entry = "decode_select_token"  # what {t-1} resolves to at t = 1

  [[chain.repeat.stage]]
  name = "decode_embed_{t}"
  project = "input-embedding"
  inputs.transcript = { from = "decode_select_{t-1}" }
  inputs.embedding  = { input = "embedding" }

  [[chain.repeat.stage]]
  name  = "decode_range_{t}_l{l}"
  index = "l"                  # inner fan, static count
  count = 35
  project = "prefill-range"
  inputs.activations = { from = "decode_range_{t}_l{l-1}", first = "decode_embed_{t}" }
  inputs.cache       = { from = "decode_range_{t-1}_l{l}", first = "prefill_range_l{l}" }
  inputs.ple         = { from = "decode_aux_{t}_l{l}" }
  inputs.layer       = { input = "layer_{l}" }
```

Outside the block, `{last}` names the final iteration:

```toml
[[chain.stage]]
name = "response_detokenize"
inputs.transcript = { from = "decode.{last}.decode_select_{t}" }
```

Templating rules:

- `{ident}` substitutes a bound index. Indexes are `u32`, bound by the enclosing `index =`
  declarations; an unbound name in a template is an expansion error.
- `{ident-1}` is the only arithmetic. It exists to express the sequential dependency
  that makes a repeat a chain rather than a fan-out, and deliberately stops there —
  general expressions in a manifest is the road to a config language.
- **A binding whose template contains `{ident-1}` MUST declare `first =`.** That is the
  underflow case, and it is exactly the edge that mis-wires when hand-written. Omitting
  it is an expansion error, not a default.
- `{last}` outside the block resolves to `count`; when `count == 0` the block contributes
  no stages and `{last}` resolves to `entry`. `count = 0` is a real case, not a
  degenerate one — a zero-token generation request must still detokenize the prompt.
- Expansion order is: outer index ascending, then `[[chain.repeat.stage]]` declaration
  order, then inner index ascending. Fixed, because `InputBindingSource::Chained` is
  positional.

### 3. Count sources

```toml
count = 35                                                        # literal
count = { input = "sampling", select = "max_new_tokens", max = 128 }   # external
count = { from = "plan_generation", select = "steps", max = 128 }      # stage output
```

`select` is a `select!`-style path into the named value. The selected value MUST be an
unsigned scalar (`u8`/`u16`/`u32`/`u64`); a signed or non-scalar selection is a manifest
error. This is `dynamic-index-selection.md`'s rule verbatim, and for the same reason.

`max` is **mandatory** on the non-literal forms. It bounds expansion from the manifest
alone, before any value is read, so a hostile or corrupt input cannot ask for 10⁹ stages.
A resolved count exceeding `max` aborts the run; it does not silently clamp, because
clamping would make two different honest inputs produce the same chain.

### 4. Two-phase execution

`resolve_chain` currently returns a fully-known `ChainSpec`. It becomes:

1. **Partition.** Split the manifest at the first repeat block whose count is
   `{ from = ... }`. Everything before it is statically known.
2. **Run the prefix.** Execute those stages exactly as today.
3. **Resolve.** Read the count-producing stage's `output.bin` + `output_manifest.json`,
   apply `select`, obtain `n`. Check `n <= max`.
4. **Expand.** Materialize the block's stages with `t = 1..=n`, splice them in, and
   continue. Repeat from (1) if a later block's count also depends on a stage output.

Everything downstream — stage running, checkpoint recording, `chain audit`, the fraud
machinery — operates on the expanded `Vec<StageSpec>` and is unchanged.

**Structural rule:** a `count = { from = "S" }` block may only name a stage `S` that
precedes it in declaration order, and `S` may not itself be inside that block or any
later one. Checked at manifest load, before anything runs. Without it, a chain could
declare a count that depends on the stages the count creates.

### 5. What gets recorded

`ChainCommitment` gains a shape record, so a verifier can check the graph as well as the
links:

```rust
pub struct ChainCommitment {
    pub stages: Vec<StageCheckpoint>,
    pub shape: ChainShape,                        // new
}

pub struct ChainShape {
    /// sha256 of the *unexpanded* manifest bytes — the template, not the result.
    pub manifest_digest: Vec<u8>,
    pub repeats: Vec<RepeatResolution>,
}

pub struct RepeatResolution {
    pub name: String,
    /// Which stage produced the count (index into `stages`), or `None` for a
    /// literal / external source.
    pub source_stage: Option<u32>,
    /// The external's declared commitment, for a `{ input = ... }` source.
    pub source_commitment: Vec<u8>,
    pub selector: String,
    pub max: u32,
    pub resolved_count: u32,
}
```

`ChainCommitment::digest()` is unchanged in form — it hashes the postcard of the whole
struct, so the shape is covered automatically.

### 6. Verification

This is the part that has to be right, so it is stated as the verifier's algorithm.

Given `ChainCommitment`, the unexpanded manifest, and each stage's `output.bin`:

1. Check `sha256(manifest bytes) == shape.manifest_digest`. The template is pinned.
2. For each `RepeatResolution`:
   - **literal / external source** — re-derive the count from the manifest, or from the
     external whose commitment the manifest declares. Both are manifest-static.
   - **stage source** — take `stages[source_stage]`, which is an already-verified
     checkpoint naming a program, its inputs, and `output_structural_commitment`. Read
     that stage's `output.bin`, confirm its structural root equals the checkpoint's
     commitment, apply `selector`, and obtain `n`.
   - Assert `n == resolved_count` and `n <= max`.
3. Re-expand the manifest with the verified counts. Compare stage-for-stage against
   `ChainCommitment.stages` — the existing `crates/raster-cli/src/chain.rs:371` check,
   now run against a derived rather than a declared length.
4. Verify links and identities per `program-chain.md`, unchanged.

**Why a stage-produced count is sound.** The concern is that a runtime-derived trip count
lets whoever executes the chain choose the shape — that a run truncated to 3 iterations
would be indistinguishable from an honest 3-iteration run. It is not, because step (2)
re-derives the count from an *authenticated* artifact: the count-producing stage has its
own checkpoint, its own program identity, and its own output commitment, all verified
before the count is read. A prover who emits 7 and expands to 3 produces a
`ChainCommitment` that fails step (3). A prover who expands to 3 and *claims* 3 must have
a stage checkpoint whose committed output says 3, which is a claim about that stage's
execution — refutable by an ordinary `Execution` fraud proof.

What this does **not** give is liveness: nobody can force a party to produce a chain run
at all, and a party that abandons a run mid-expansion simply has no `ChainCommitment` to
show. That is already true of every stage in the chain and is not a new exposure.

What it does newly require is the **bound**, because unlike a static manifest the work is
not knowable from the template alone. `max` supplies it.

### 7. Repeat blocks and per-stage constants

The `raster-inference` decode block exposes a wiring question worth settling in this
proposal, because it will recur: Gemma 3n's last twenty layers attend over layer 13's or
layer 14's KV cache, per layer, from model config.

The tempting answer is a lookup table in the manifest (`donor_map = { 15 = 13, 19 = 14,
… }`). It should be rejected. `score_key` in `raster-inference` gates on
`(params.kv_donor_layer >= 0) != donor_pass` — a **boolean**: the committed layer params
say *whether* a layer shares, and the manifest alone says *with whom*. Nothing
cross-checks them, so a wrong donor edge produces wrong activations with a clean audit.
That exposure exists in the hand-written manifest today, across twenty stages.

The rule this proposal adopts: **the manifest carries topology; committed externals carry
model facts.** Where a per-iteration constant comes from the model, bind all candidates
uniformly and let the committed params select:

```toml
inputs.donor_kv_sliding = { from = "decode_range_{t}_l13" }
inputs.donor_kv_full    = { from = "decode_range_{t}_l14" }
```

Identical for all 35 layers, no table, and a wrong donor stops being expressible. The
cost is one extra key sweep per layer that self-skips — the shape the existing two-pass
code already pays.

This keeps the templating language small, which is the point: every irregularity that
*can* be pushed into a committed external is one the manifest does not need syntax for.

### 8. Future work: `while` mode

The count here is resolved once, before the block runs. `raster-inference`'s actual
stopping condition is EOS, which is not knowable until iteration *t* has run.

The extension is a per-iteration continuation flag:

```toml
[[chain.repeat]]
name  = "decode"
index = "t"
while = { from = "decode_select_{t}", select = "continue", max = 128 }
```

Expansion becomes incremental — run iteration *t*, read its flag, decide whether to emit
*t+1* — and `RepeatResolution` grows a per-iteration decision list. The verification
argument is the same one as §6, applied *n* times instead of once: each decision is
re-derived from an authenticated output the verifier already holds.

It is deliberately not in this proposal. It changes the runner from "expand, then
execute" to "interleave," which touches the run loop rather than just the manifest layer,
and it wants `chain-fraud-proof`'s attribution story extended to a fault of the form "the
decision to continue at iteration 5 was wrong." Both are tractable; neither should be
bundled with getting the templating and the shape record right.

Also deferred:

- **Parallel fan-out.** Within a decode step the 35 `decode_aux_{t}_l{l}` stages are
  independent (all read `decode_embed_{t}`), while the 35 `decode_range` stages form a
  chain. The bindings already distinguish them, so a scheduler could exploit it without
  new syntax — but the runner is serial today and this proposal does not change that.
- **Multi-output stages.** Inherited from `program-chain.md` v1.

## Implementation plan

1. **`raster-cli`, manifest layer** — `ChainTable` gains `input` and `repeat`;
   `InputBinding` gains a `Named` variant; new `expand.rs` holding the template
   substitution, the structural rules (acyclicity, mandatory `first`, mandatory `max`,
   unsigned selector), and the deterministic ordering. Output is a plain
   `Vec<StageSpec>`, so nothing downstream changes shape.
2. **`raster-cli`, run loop** — partition/resolve/expand per §4. The value read in step
   (3) reuses the existing external-resolution path, since a stage output is
   format-compatible with an external input.
3. **`raster-core`** — `ChainShape` / `RepeatResolution`; `ChainCommitment.shape`. This
   is a chain-commitment format break; `chain-commitment` files do not survive it.
4. **`raster-cli`, audit** — the §6 algorithm; `:371` moves behind re-expansion.
5. **`chain_fraud` guest** — `ChainFaultKind::Shape`, evidence being the count-producing
   stage's output bytes plus the selector, with the guest re-deriving and comparing
   against `resolved_count`.
6. **Acceptance** — `raster-inference`: express the 35 prefill layer stages as a static
   repeat (pure refactor; the expanded manifest must be byte-identical to today's
   hand-written one and produce an unchanged chain digest), then add the decode block
   with a stage-produced count.

Step 6's first half is the real test of the templating design: if `[[chain.repeat]]`
cannot reproduce the existing 74-stage manifest exactly, the syntax is wrong.

## Open questions

- **Should `{last}` be spelled positionally at all?** `decode.{last}.decode_select_{t}`
  is unpleasant. An alternative is letting a repeat block declare named exports
  (`exports.transcript = "decode_select_{t}"`) that outside stages bind as
  `{ from = "decode.transcript" }`, meaning the final iteration's. Cleaner to read,
  more machinery.
- **Should the shape record pin the expansion, or just the counts?** §5 records counts and
  re-derives stages. Recording an expansion digest as well would let a verifier detect a
  template/expansion mismatch without re-implementing substitution — at the cost of a
  second thing that can disagree.
- **Nested stage-produced counts.** §4 handles them by iterating partition/resolve/expand,
  but a block whose *inner* count comes from a stage inside the outer block is a case the
  structural rule currently forbids outright. Whether that is too strict is unclear
  without a second use case.
