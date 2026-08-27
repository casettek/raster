# Proposal: `chain-repeat` — repeated chain segments with an authorized trip count

Status: **implemented** (2026-08-27) — proposed 2026-08-19, revised the same day against the
landed half of [`chain-io-commitment.md`](./chain-io-commitment.md). See §Implementation status.

Companion to: [`program-chain.md`](./program-chain.md) (partly implemented),
[`chain-fraud-proof.md`](./chain-fraud-proof.md) (implemented),
[`chain-stage-execution.md`](./chain-stage-execution.md) (partly implemented) — supplies
`chain run --stage <name>`, which §4's expansion must keep addressable.

Rests on: [`chain-io-commitment.md`](./chain-io-commitment.md) (**partly implemented**,
2026-08-27). Its phases 1–4 landed *after* this proposal was written and moved three things
underneath it:

- `StageCheckpoint` no longer carries `trace_commitment_digest` — a checkpoint is I/O only.
- `ChainFaultKind::Execution` is **removed**; `Link` is the only fault a `ChainCommitment`
  condemns on its own (`crates/raster-core/src/chain.rs:75-86`).
- A chain commitment is now producible by a **cheap** run — both postures build checkpoints.

§6's soundness argument is restated against that world and §6.1 is new. The design itself is
unchanged; what changed is which mechanism settles a dishonest trip count, and how much a
repeat-expanded chain costs to commit to.

Precedent: [`dynamic-index-selection.md`](./dynamic-index-selection.md) (phases 1–3
implemented) — the same "the index must be an authorized value" argument, one level up.

## Problem

A `[chain]` manifest is a flat, hand-written `[[chain.stage]]` list
(`crates/raster-cli/src/chain.rs:57`, `:64`). That is fine when the pipeline's shape is a
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
  (`crates/raster-cli/src/chain.rs:57`) via `load_spec` (`:1681`), from either a `Raster.toml`
  `[chain]` table or a `chain.json`. Both formats land on the same `StageSpec` /
  `InputBinding` shapes (`:64`, `:76`), so a change made at the `ChainSpec` level is
  format-agnostic — and anything this proposal *commits to* must therefore be defined at the
  `ChainSpec` level, not over manifest bytes (§5).
- `InputBinding` is `External(ExternalRef)` or `From(String)`, where `ExternalRef` carries
  `{ path, index_path, commitment }` — the commitment is a **manifest constant** today,
  never read from file bytes.
- `ChainCommitment { stages: Vec<StageCheckpoint> }` (`crates/raster-core/src/chain.rs:60`)
  is the verifier-facing object, and `StageCheckpoint` (`:40`) names `program_commitment`,
  `input_manifest_commitment`, `input_bindings`, `output_payload_commitment`, and
  `output_structural_commitment` per stage — **six fields, all pure functions of public
  artifacts.** A verifier holding it plus each stage's `output.bin` checks every link with no
  prover. `trace_commitment_digest` was the seventh and was removed by
  `chain-io-commitment.md` phase 4; nothing in a checkpoint names a trace any more.
- `InputBindingSource::Chained { stage: usize }` (`crates/raster-core/src/chain.rs:24`)
  refers to a producing stage **by index into the expanded list**. Expansion order is
  therefore load-bearing and must be deterministic.
- `chain audit` compares recorded against declared shape by length first —
  `if chain.stages.len() != spec.stages.len()` (`crates/raster-cli/src/chain.rs:527`) —
  then per-stage by name. This is the check that must not become circular once the length is
  derived rather than declared.
- `ChainFaultKind` is now **`{ Link }`** (`crates/raster-core/src/chain.rs:75`) — a single
  variant, documented as "the only fault a `ChainCommitment` can condemn *on its own*",
  because with no trace commitment in the checkpoint there is nothing for a stage fraud
  receipt to be attributed against. `Link` is "an inconsistency inside the `ChainCommitment`
  itself, proven from the manifest the checkpoint committed": in-proof, self-contained, no
  trace. **That is now the admission criterion for adding a variant, and a shape fault meets
  it** — see §6.1. It is the same category of object, which is why it still extends an
  existing pattern rather than inventing one.
- **Both postures build checkpoints.** `chain run --no-auth` writes a real `ChainCommitment`
  (`chain-io-commitment.md` §Implementation status), degrading only when program identity is
  unresolvable. So §4's partition/resolve/expand loop must work unauthenticated — which is
  also what makes a 1,535-stage decode chain affordable to commit to at all (§6.1).
- **The output-fraud scan runs inside the claimer's declared shape.** `detect_output_fraud`
  (`crates/raster-cli/src/chain.rs:693`) walks the recorded checkpoints and re-runs each stage
  from **that stage's own recorded `input.json` / `input_manifest.json`** — not from a fresh
  whole-chain resolution. An auditor who disagrees about the shape therefore does not
  accidentally compare against a differently-expanded chain; shape is a separate question,
  settled separately (§6.1).
- `chain run --stage <name>` re-runs one stage in place, in either posture (the `--no-auth`
  clap gate was lifted by `chain-io-commitment.md` phase 3). Stage names resolve against the
  **spec**, up front (`chain.rs:145-150`) — so under repeats they must resolve against the
  *expanded* spec, which §4 has to be able to produce without executing anything.
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

**The indexed form.** A repeat block usually binds a per-iteration external — 35 layer files,
each with its own commitment. Those 35 commitments are distinct hashes and must appear in the
manifest either way; what does not need repeating is the path and the surrounding block:

```toml
[chain.input.aux_layer]
index       = "l"
path        = "prefill-prepare-aux/layer{l}.rastered"
index_path  = "prefill-prepare-aux/layer{l}.rindex"
commitments = [
  "f85c872eee…",   # l0
  "e7e4703f7e…",   # l1
  # … 35 entries, positional
]
```

This is **sugar, not a new kind**: at load it flattens into plain named inputs
`aux_layer_0 … aux_layer_34`, `count` is the array's length, and a binding refers to one as
`{ input = "aux_layer_{l}" }` — ordinary textual substitution against an ordinary name, so
there is no new reference syntax and nothing downstream distinguishes the two forms. The
flattened result is what `spec_digest` covers.

Keeping every commitment written out, positionally, is deliberate: a per-index commitment is
the one thing in this table a verifier must read, and compressing it is the only compression
that would cost something.

### 2. The repeat block

**Stages and repeats interleave, and the order is consensus-critical.** A chain is
`stage, stage, repeat, repeat, stage` — `raster-inference` is exactly that shape — and
`InputBindingSource::Chained { stage: usize }` records producers *by index into the expanded
list*, so a manifest that cannot state the relative order of a `[[chain.stage]]` and a
`[[chain.repeat]]` cannot be expanded deterministically.

TOML does not give that order for free: `[[chain.stage]]` and `[[chain.repeat]]` are two
independent arrays-of-tables, and serde hands back two independent `Vec`s. The loader
therefore deserializes both wrapped in `toml::Spanned` and merge-sorts by source span into
one **normalized item list**:

```rust
enum ChainItem { Stage(StageSpec), Repeat(RepeatSpec) }
```

The JSON form carries a single tagged `items` array, which is already ordered. Both formats
normalize to the same `Vec<ChainItem>`, and that normalized list — not either file's bytes —
is what §5's `spec_digest` covers. This is the same argument §5 makes for digesting the parse
rather than the file, arriving one level earlier and as a hard requirement rather than a
preference.

```toml
[[chain.repeat]]
name  = "decode"
index = "t"                    # bound to start..start+count inside the block
count = { from = "plan_generation", max = 128 }

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

A block declares what outside stages may bind to. Names, not positions — each export names
the stage it resolves to at the final iteration, and the stage it falls back to when the
block runs zero times:

```toml
  [chain.repeat.exports.transcript]
  stage = "decode_select_{t}"          # the final iteration's
  entry = "prefill_select_token"       # when count == 0
```

```toml
[[chain.stage]]
name = "response_detokenize"
inputs.transcript = { from = "decode.transcript" }
```

This replaces the positional `decode.{last}.decode_select_{t}` spelling an earlier draft
used, which referenced `{t}` outside the block that binds it — contradicting the
unbound-index rule below. It is also what makes the `count == 0` fallback well-defined for a
block that emits several stages per iteration: one `entry` per export, rather than one per
block for many possible referents.

Templating rules:

- `{ident}` substitutes a bound index. Indexes are `u32`, bound by the enclosing `index =`
  declarations; an unbound name in a template is an expansion error.
- **Indexes are 0-based, over `start .. start + count`, with `start` defaulting to 0.**
  `count = 35` binds `l = 0..34`, matching the `l0`-based naming every layer stage already
  uses. `start` exists because a segment sometimes begins partway: layers 15–34 are a
  different wiring regime from 0–14 (§7), and `start = 15, count = 20` is how that is said.
  Note `count` counts *iterations* and `max` bounds *`count`*, not the largest index — error
  messages must say which.
- `{ident-1}` is the only arithmetic. It exists to express the sequential dependency
  that makes a repeat a chain rather than a fan-out, and deliberately stops there —
  general expressions in a manifest is the road to a config language.
- **A binding whose template contains `{ident-1}` MUST declare `first =`.** `{ident-1}`
  underflows at `ident == start`, and that is exactly the edge that mis-wires when
  hand-written. Omitting `first` is an expansion error, not a default — including when
  `start > 0` and `{ident-1}` would *happen* to name a real earlier stage. The rule stays
  one line, and `first` stays the block's explicit entry edge.
- **At most one `{ident-1}` per template.** `decode_range_{t-1}_l{l-1}` would need two
  fallbacks and `first` has one slot; rather than making `first` a map, the form is refused.
  Nothing in `raster-inference` needs two.
- An export resolves to iteration `count`; at `count == 0` the block contributes no stages
  and each export resolves to its own `entry`. `count = 0` is a real case, not a degenerate
  one — a zero-token generation request must still detokenize the prompt.
- **Substitution is textual; names are resolved after expansion.** `{ident}` is replaced in
  the string, and the resulting flat list is checked by the ordinary `validate_spec` rules
  (producer exists, producer runs earlier). Nothing about a name is special because it came
  from a template.
- **Interior names of a block whose count is *not* manifest-static are unaddressable, from
  anywhere.** That is the real restriction: such a name's existence depends on a value read
  at run time, so binding to it is not checkable from the manifest. A block with a literal
  count exposes every interior name, because those names are a function of the manifest
  alone — and that is load-bearing, not a concession: `prefill_range_l{l}` binds
  `ple = { from = "prefill_prepare_aux_l{l}" }`, an interior name of a *different* static
  block, indexed by the consumer's own index. Exports exist so a *dynamic* block has
  something stable to expose; they are not a general access rule.
- Expansion order is: outer index ascending, then `[[chain.repeat.stage]]` declaration
  order, then inner index ascending. Fixed, because `InputBindingSource::Chained` is
  positional.

### 3. Count sources

```toml
count = 35                                                             # literal
count = { input = "sampling", select = "max_new_tokens", max = 128 }   # external
count = { from = "plan_generation", max = 128 }                        # stage output
```

The resolved value MUST be an unsigned scalar (`u8`/`u16`/`u32`/`u64`), and must fit `u32`
after resolution; a signed selection, a non-scalar one, or a `u64` above `u32::MAX` is an
error, not a clamp. This is `dynamic-index-selection.md`'s rule verbatim, and for the same
reason.

`select` is a `select!`-style path, and it is available **only on the `{ input = ... }`
form**. That asymmetry is deliberate and it is the one design change this revision makes:

> **A stage-produced count is the producing stage's whole output.** The stage's `main`
> returns `uN`, and nothing selects into it.

The reason is §6.1. A shape fault has to be checkable in-guest to sit beside `Link`, and
`verify_link_fault` is built around the discipline that the guest exhibits a fault
*"without parsing JSON in-guest"* (`crates/raster-prover/guests/chain_fraud/src/main.rs`).
Applying a `select!` path to a `.rastered` payload in-guest is strictly more decoding than
any fault kind does today, and it introduces a host/guest decoder-divergence surface that
nothing else in the chain layer has.

With the whole-output rule the guest does not decode at all — it **re-encodes**. A scalar
payload's structural root is one hash of its little-endian bytes under a `b"leaf"` domain tag
(`crates/raster-core/src/input.rs:634`, and `u64_leaf_root` at `:1171` is already that
function for `u64`), so the check is:

```
scalar_leaf_root(width, n) == stages[source_stage].output_structural_commitment
```

One hash against a field the checkpoint already carries. The direction matters: the guest
computes the commitment a count *would* have and compares, rather than parsing a payload to
find one — so a malformed or adversarial `output.bin` has no path into the guest, and there
is no decoder to diverge from the host's.

`scalar_leaf_root(width, value)` is the whole of the `raster-core` change: `IndexWidth` and
`IndexWidth::encode` (`input.rs:56-77`) are already `pub`, already serializable, and already
do fixed-width LE returning `None` on overflow rather than truncating — so the new function
is a thin `pub` wrapper, and today's private `u64_leaf_root` (`:1171`) reduces to a call.

An external count keeps `select` because that form is manifest-static: it is re-derived from
bytes whose commitment the manifest declares, outside any guest, and never reaches
`chain_fraud`.

The cost is one trivial adapter stage when a planner also computes other things. That is
cheaper than a decoder in the guest, and it keeps `chain_fraud` at `env::verify` plus hash
equality — the posture that made `Link` the one fault a `ChainCommitment` still condemns on
its own.

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

**Expansion must also exist as a pure function**, separate from the run loop:

```rust
fn plan(manifest: &ChainManifest) -> Result<ExpansionPlan>       // static prefix length
fn verify_shape(manifest, chain) -> Result<VerifiedCounts>       // §6 steps 1-2
fn expand(manifest: &ChainManifest, counts: &VerifiedCounts) -> Result<Vec<StageSpec>>
```

`VerifiedCounts` is a newtype whose only constructor is `verify_shape`. That makes the
"already verified before they are used" claim below mechanical rather than documentary:
`expand` is uncallable without one.

Given the manifest and the already-resolved counts, `expand` produces the expanded list
without executing anything. Three callers need exactly that and none of them may run a
prefix:

- `chain audit`, at §6 step 3, re-deriving the length the `:527` check compares against.
- `chain run --stage <name>`, whose `stage_index` is built from the spec up front
  (`chain.rs:145-150`) — under repeats the addressable names are the expanded ones
  (`decode_range_7_l13`), and the stage being re-run is precisely the one whose producers
  have already run.
- `detect_output_fraud`, which zips `spec.stages` against `chain.stages` (`chain.rs:693-700`)
  and today would see a template list against an expanded one.

For all three the counts come from `ChainShape.repeats[].resolved_count` in the commitment —
already verified by §6 step 2 before they are used, so this is not trust, it is ordering.

**The run loop uses the same `expand`, always from scratch, and never splices.** Step (4)
above re-expands the whole manifest from the counts resolved so far and asserts that the
already-executed prefix of the new list is element-wise equal to what ran; execution then
continues at the cursor. Expansion cannot re-run anything because it does no I/O, and the run
loop cannot re-run anything because the cursor only moves forward. That equality assertion is
what keeps the positional `Chained { stage }` indices in already-written checkpoints provably
correct as the list grows.

A dynamic chain also writes `chain_dir/chain-shape` as each count resolves, **unconditionally
— not gated on whether a chain-commitment is being written.** `run` degrades to no commitment
when program identity is unresolvable (`chain.rs:174-192`), and that is precisely the state a
contested chain can be in, so `--stage` name resolution must not depend on the commitment
existing. It also makes the partition loop resumable.

Both postures run this loop. `chain-io-commitment.md` made a cheap run produce a real
`ChainCommitment`, so nothing here may sit behind an authentication branch.

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
    /// `sha256(postcard(spec))` over the *unexpanded* `ChainSpec` — the
    /// template, not the result, and the decoded spec, not the file.
    pub spec_digest: Vec<u8>,
    pub repeats: Vec<RepeatResolution>,
}

pub struct RepeatResolution {
    pub name: String,
    /// Which stage produced the count (index into `stages`), or `None` for a
    /// literal / external source.
    pub source_stage: Option<u32>,
    /// The external's declared commitment, for a `{ input = ... }` source.
    pub source_commitment: Vec<u8>,
    /// The `select!` path — non-empty only for an `{ input = ... }` source
    /// (§3: a stage-produced count is the producing stage's whole output).
    pub selector: String,
    /// The unsigned width the count-producing stage's `main` returns. A leaf's
    /// bytes are fixed-width little-endian and never widened, so `7u32` and
    /// `7u64` are different payloads with different roots — the width is not
    /// derivable from the count and must be recorded, or §6 step 2 cannot be
    /// computed at all. `IndexWidth` (`raster-core::input:56-77`) is already
    /// the `pub`, serializable encoder for exactly this.
    pub width: IndexWidth,
    pub max: u32,
    pub resolved_count: u32,
}
```

`ChainCommitment::digest()` is unchanged in form — it hashes the postcard of the whole
struct, so the shape is covered automatically.

**Why `postcard(ChainSpec)` and not `sha256(manifest bytes)`.** An earlier draft digested the
manifest file. That is wrong for the reason §Facts already records: `load_spec` accepts a
`Raster.toml` `[chain]` table *and* a `chain.json`, and both land on the same `ChainSpec`
(`chain.rs:1681`). Hashing bytes would mean a verifier holding the JSON form of a chain
authored in TOML cannot run §6 step 1 at all, and that a whitespace or comment edit moves the
chain digest. Worse for soundness: `sha256(bytes)` pins an encoding, not a parse — it never
establishes that the verifier's decoded spec is the one the claimer expanded from. A digest
over the canonical postcard of the decoded spec pins the thing the expansion is actually a
function of, and reuses `ChainCommitment::digest()`'s own pattern.

**Not `Option<ChainShape>`.** The tempting reason to make it optional is digest stability for
chains with no repeat block, and that reason is void: postcard is non-self-describing, so
`None` still encodes a byte and a repeat-less chain's digest moves *identically* either way.
Existing `chain-commitment` files fail to decode under both. With no benefit left, the cost
decides it — an optional field gives a claimer a "no shape recorded" posture that a verifier
must then decide how to treat, and any treatment other than rejection is a downgrade attack
on the S1 closure below. Unconditional.

**A consequence worth stating: expansion is not digest-neutral.** Two manifests that expand to
the same stage list — one written out, one using a repeat block — produce **identical
`StageCheckpoint`s** and a **different chain digest**, because the digest covers `spec_digest`
and their manifests differ. That is the intended reading: the digest names the chain *as
authored*, which is precisely what makes the next paragraph true. An implementation should pin
both halves — checkpoints equal, digest not — so neither is later mistaken for the other.

**This closes S1, and the other proposals should say so.** `chain-fraud-proof.md` and
`chain-io-commitment.md` both carry forward *"the `ChainCommitment` does not commit to the
chain spec … a `chain_spec_commitment` remains future work"* (`chain-io-commitment.md` S1).
`spec_digest` is that commitment, and `ChainShape` is present on every chain, repeats or not.
Landing this proposal therefore closes S1 as a side effect; mark it closed in both documents
rather than leaving three places disagreeing about whether it is open.

### 6. Verification

This is the part that has to be right, so it is stated as the verifier's algorithm.

Given `ChainCommitment`, the unexpanded manifest, and each stage's `output.bin`:

1. Check `sha256(postcard(spec)) == shape.spec_digest`, where `spec` is the verifier's own
   decode of the manifest it holds. The template is pinned (§5).
2. For each `RepeatResolution`:
   - **literal / external source** — re-derive the count from the spec, or from the external
     whose commitment the spec declares, applying `selector`. Both are manifest-static: no
     stage artifact is involved and nothing is read from a run.
   - **stage source** — take `stages[source_stage]`, an already-verified checkpoint naming a
     program, its inputs, and its output commitments. The payload **is** the count (§3), so
     assert `scalar_leaf_root(width, resolved_count)` equals that checkpoint's
     `output_structural_commitment`. Nothing is parsed: the recorded count is
     re-encoded and its commitment compared. Outside a guest, `output.bin` is available too
     and `sha256` of it against `output_payload_commitment` is the same check by another
     route.
   - Assert `n == resolved_count` and `n <= max`.
3. Re-expand the spec with the verified counts (§4's `expand`). Compare stage-for-stage
   against `ChainCommitment.stages` — the existing `crates/raster-cli/src/chain.rs:527`
   check, now run against a derived rather than a declared length.
4. Verify links and identities per `program-chain.md`, unchanged.

Steps 1–3 need no prover, no trace, and no re-execution. They read the manifest, the
commitment, and one `output.bin` per stage-sourced repeat — all artifacts a relying party
already holds under `program-chain.md`'s availability assumption.

**Why a stage-produced count is sound.** The concern is that a runtime-derived trip count
lets whoever executes the chain choose the shape — that a run truncated to 3 iterations
would be indistinguishable from an honest 3-iteration run. It is not, and the argument
splits cleanly in two:

- **The shape is settled in-proof.** Step (2) re-derives the count from an *authenticated*
  artifact: the count-producing stage has its own checkpoint, its own program identity, and
  its own output commitment, all verified before the count is read. A prover who commits 7
  and expands to 3 produces a `ChainCommitment` that fails step (3). This half is decisive,
  cheap, and self-contained — the same category of object as `Link`.
- **The count's *value* is settled optimistically.** A prover who expands to 3 and *commits*
  3 must have a stage checkpoint whose `output_payload_commitment` says 3. That is a claim
  about that stage's execution, and it is refuted the way every execution claim is now
  refuted.

The second bullet is what `chain-io-commitment.md` changed. An earlier draft of this
proposal ended it *"refutable by an ordinary `Execution` fraud proof"* — that variant no
longer exists (`ChainFaultKind` is `{ Link }`). What replaces it is §6.1.

What this does **not** give is liveness: nobody can force a party to produce a chain run
at all, and a party that abandons a run mid-expansion simply has no `ChainCommitment` to
show. That is already true of every stage in the chain and is not a new exposure.

What it does newly require is the **bound**, because unlike a static manifest the work is
not knowable from the template alone. `max` supplies it.

### 6.1 Where a shape fault sits among the others

`chain-io-commitment.md` reorganized how a chain is condemned, and a trip count has to be
placed inside that ordering rather than beside it. Three mechanisms, cheapest and most
decisive first:

| | fault | mechanism | cost | needs settlement? |
|---|---|---|---|---|
| 1 | **Shape** — `resolved_count` disagrees with the count stage's committed output | §6 steps 1–3, in-proof; `ChainFaultKind::Shape` | one hash, no payload | no |
| 2 | **Link** — a `from` parameter's committed input ≠ the producer's committed output root | `detect_link_fraud` + the authorization journal, unchanged | one authorization receipt | no |
| 3 | **Output** — a stage's committed output ≠ honest execution | `detect_output_fraud` → terminal-window receipt; `chain-io-commitment.md` §3's challenge/response | a stage re-run + one window proof | **yes** |

**The order is not cosmetic.** Shape must be resolved before Output, because Output is only
meaningful relative to a fixed stage list. Resolving it first is free, since (1) reads
artifacts the relying party already holds and reaches a verdict with no clock and no
counterparty.

Two properties make this compose rather than collide:

- **A `Shape` fault is admissible under the new criterion.** `ChainFaultKind` narrowed to
  `Link` because a checkpoint no longer names a trace, so there was nothing for an execution
  receipt to be attributed against. `Shape` has the opposite property: it is re-derived
  entirely from the `ChainCommitment` itself, with no trace and — given §3's whole-output
  rule — no artifact either: the guest re-encodes the recorded count and compares one hash
  against a checkpoint field. Strictly *less* machinery than `Link`, which verifies an
  authorization receipt. The composed enum is `{ Link, Shape }`.
  `chain-io-commitment.md` §5 was written without this proposal in view and would delete the
  variant by omission if the two land in the wrong order; see §Implementation plan.
- **The output scan already runs inside the claimer's shape.** `detect_output_fraud`
  (`chain.rs:693`) re-runs each stage from *that stage's recorded* `input.json` /
  `input_manifest.json`, not from a fresh whole-chain resolution. So an auditor who believes
  the count is wrong does not silently end up comparing a 3-iteration run against a
  7-iteration one — which would make every downstream stage's `input_manifest_commitment`
  differ and, once `chain-io-commitment.md` §3 lands, would fail its admission check **H3**
  with an honest accusation. The existing code has the right behaviour; what this proposal
  adds is the requirement that it stay that way, and that the scan zip against the
  *expanded* spec (§4).

**What a repeat block costs to commit to, now.** This is the other half of what changed
underneath. A chain commitment used to require an authenticated run of every stage — 6.6× on
`hello-tiles`. For a 20-token decode chain at 1,535 stages, that tax fell on a structure this
proposal exists to make expressible, which was an awkward place for the motivating example to
sit. Phase 4 of `chain-io-commitment.md` removed it: both postures build checkpoints, every
input to §6's verification is posture-independent, and `ChainShape` is recorded either way. A
repeat-expanded chain is now committable cheaply and verifiable in full. The one place the
authenticated posture is still required is disputing a *particular* stage, which is one
stage, on demand — exactly what `chain run --stage <name>` re-runs in place, and §4's
`expand` is what makes that stage addressable by name.

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

**The prefill side is the same problem, already live, and it is the reason this section is
not optional.** `raster-inference`'s hand-written `prefill_range_l0..l34` wires `donor_kv` to
`input_embedding` for layers 0–14, to `prefill_range_l13` for 15–34 — *except* layers 19, 24,
29 and 34, which use `l14`. That is a four-entry lookup table living in a manifest, in the
one place nothing cross-checks it, exactly as described above.

Two consequences for anyone collapsing that block:

- **It is unexpressible until `prefill-range` takes two donor bindings.** No amount of
  templating says "except 19, 24, 29, 34", and adding syntax that does is the rejected
  design.
- **Even then it needs two blocks, not one.** Layers 0–14 and 15–34 are different wiring
  regimes, and a single 35-iteration block binding `{ from = "prefill_range_l13" }` is a
  *forward* reference for `l ≤ 12` and a *self* reference at `l = 13` — which `validate_spec`
  rejects on the expanded list, correctly. The shape is:

  ```toml
  [[chain.repeat]]  index = "l"  start = 0   count = 15   # donor: input_embedding
  [[chain.repeat]]  index = "l"  start = 15  count = 20   # donor: the two committed candidates
  ```

  The second block's `activations = { from = "prefill_range_l{l-1}", first = "prefill_range_l14" }`
  still declares `first`, even though at `start = 15` the `{l-1}` form happens to name a real
  stage. The rule is unconditional for a reason: an implementer who reasons "it doesn't
  underflow here" has just made the entry edge implicit.

`prefill_prepare_aux_l0..l34`, by contrast, is uniform across all 35 layers and collapses
with no program change. That asymmetry is why the two blocks are sequenced apart: the aux
block is the acceptance case for the templating design, and the range block is the
acceptance case for *this section*.

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

§6.1's split carries over and is worth stating, because it is what keeps the deferral cheap.
Each decision's *shape* consequence stays in-proof — read iteration *t*'s committed output,
compare against the recorded decision — so `while` mode does not multiply the settlement
surface by *n*. Only the honesty of those *n* outputs is optimistic, and that is already
*n* ordinary stages' worth of exposure, not a new kind.

It is deliberately not in this proposal. It changes the runner from "expand, then
execute" to "interleave," which touches the run loop rather than just the manifest layer,
and it wants `chain-fraud-proof`'s attribution story extended to a fault of the form "the
decision to continue at iteration 5 was wrong." Both are tractable; neither should be
bundled with getting the templating and the shape record right.

Also deferred:

- **Collapsing `prefill_range` (§7).** Blocked on a `prefill-range` program change — two
  donor bindings selected by committed layer params — plus the two-block split above. It is
  the natural second adopter of this feature and the first real test of §7's rule, but it
  crosses into program code and regenerated commitments, so it is not bundled with getting
  the templating and the shape record right. `prefill_prepare_aux` is the acceptance case
  instead.
- **Parallel fan-out.** Within a decode step the 35 `decode_aux_{t}_l{l}` stages are
  independent (all read `decode_embed_{t}`), while the 35 `decode_range` stages form a
  chain. The bindings already distinguish them, so a scheduler could exploit it without
  new syntax — but the runner is serial today and this proposal does not change that.
- **Multi-output stages.** Inherited from `program-chain.md` v1.

## Implementation plan

Nine phases. **0–4** are additive syntax plus a pure refactor and leave the on-disk format
byte-identical, so each is independently landable. **5** is the point of no return. **6–8**
build on it.

| # | lands | exit criterion |
|---|---|---|
| **0** | Prerequisites: guard `detect_output_fraud`'s unguarded `chain.stages[i]` index (`chain.rs:693-695`, a live panic on the `fraud_prove` path); `Clone` + `PartialEq` on `StageSpec` / `InputBinding` / `ExternalRef`; `pub scalar_leaf_root(width, value)` in `raster-core::input` | `scalar_leaf_root(U32, 7) != scalar_leaf_root(U64, 7)`, and each equals `payload_structural_root` of the matching `[0x00][len][bytes]` payload |
| **1** | §1 named chain inputs — `[chain.input.<name>]`, plus the indexed form that flattens to `<family>_0 … _N` at load; `InputBinding::Named` resolving in `synthesize_inputs` (`:1375-1422`) to the same `InputBindingSource::External` an inline external produces | `examples/chain-example`'s externals re-expressed as named inputs, all 7 integration tests pass, **chain digest byte-identical**. That equality is the phase: `synthesize_inputs`'s `BTreeMap` order is what `input_manifest_commitment` hashes |
| **2** | §2's normalized item list — `Spanned` merge-sort for TOML, tagged `items` for JSON; the `ChainManifest` (unexpanded) / `Vec<StageSpec>` (expanded) type split; `RecordedChain` carries both; new `chain_expand.rs` | Existing tests pass on a zero-diff manifest; a TOML with `stage / repeat / stage / repeat` yields items in source order |
| **3** | Static expansion: `plan` + `expand`, textual substitution, `start`, `first`, one `{ident-1}`, exports, fixed ordering. Literal-count chains expand at load, so `run` and `audit` see a flat `Vec` exactly as today | **The falsification gate.** `expand(repeat_form) == hand_written_form` element-wise, with `raster-inference`'s `prefill_prepare_aux` shape as an inline fixture — the acceptance below, run with no execution and no sibling repo. If this cannot be written, the syntax is wrong and phase 5 must not start |
| **4** | §6 verification host-side: `verify_shape` + `VerifiedCounts`; the `:527` length check moves behind re-expansion; `detect_output_fraud` and `--stage` switch to the expanded list | A commitment whose `resolved_count` disagrees with the recorded `output_structural_commitment` is rejected; a stale `spec_digest` is rejected |
| **5** | **Point of no return.** §5: `ChainShape` / `RepeatResolution` (with `width`) on `ChainCommitment`. Chain-commitment format break; existing files do not survive | Full suite; every recorded digest moves, and guest image ids move with it (free in-repo — `CHAIN_FRAUD_GUEST_ID` is `risc0_build`-generated, nothing hand-pinned) |
| **6** | §4's run loop for `count = { from = "S" }`: partition / resolve / re-expand / prefix-equality assert; the structural rule at load; the `chain-shape` sidecar; identity fail-fast widened to every `project` named in the manifest | A fixture chain — a `planner` returning a bare `u32` (**new: no program in this repo returns a scalar**) feeding a repeat block feeding a sink — run in both postures, byte-identical `ChainCommitment` |
| **7** | §6.1's fault: `ChainFaultKind::Shape` beside `Link`; `ChainFraudEvidence::Shape { repeat_index }` and nothing more; guest arm; `prove_chain_fraud` takes `Option<Receipt>`; `verify_chain_fraud_receipt`'s Shape arm asserts both image-id fields empty; `detect_shape_fraud` runs first in `fraud_prove` | Tamper phase 6's `resolved_count`, `chain fraud-prove` → receipt verifies, `fault == Shape`, `faulty_stage == source_stage`, image ids empty |
| **8** | Acceptance in `raster-inference`: collapse `prefill_prepare_aux_l0..l34` to one block plus one indexed `[chain.input]`. `prefill_range` stays hand-written — see §7 | Expanded list identical to today's 74 stages, in order; digest unchanged modulo `ChainShape` |

**Two soundness constraints phase 7 must not lose.** The evidence carries `repeat_index` and
nothing else: `width`, `resolved_count`, `source_stage` and the producer's
`output_structural_commitment` all come out of `chain_commitment_bytes`, which the guest
already hashes before decoding (`guests/chain_fraud/src/main.rs:100`). `Shape` asserts an
*inequality*, exactly as `Link` does, so every input must be the claimer's own — an accuser
allowed to supply the width would submit `U8` against an honest `u64` planner and condemn an
honest chain. And the guest must assert `faulty_stage == repeats[i].source_stage`, or blame
lands on an arbitrary stage. `Shape` covers stage-sourced repeats only; a `source_stage:
None` count is manifest-static and re-derived host-side for free.

Phase 3 is the real test of the templating design: if `[[chain.repeat]]` cannot reproduce the
existing hand-written stages exactly, the syntax is wrong — and that is discoverable before
phase 5 breaks the on-disk format.

### Landing order against `chain-io-commitment`

**That proposal first, this one second** — which is now the actual state: its phases 1–4
landed 2026-08-27 and this document has been revised onto them. The residual coupling is one
line: its phase 7 narrows `ChainFaultKind` to `Link` *in the guest*, and phase 7 above adds
`Shape` to the same enum. Sequence them so `Shape` is added after the narrowing, not before,
or the narrowing removes it by omission.

Its §3 dispute protocol is **not built** and is inert without a settlement clock. Nothing in
this proposal waits on it: §6 steps 1–3 and `ChainFaultKind::Shape` are in-proof, and the
interim `chain fraud-prove` evidence receipt covers the output half exactly as it does for a
hand-written chain. A stage-produced trip count inherits that proposal's settlement
dependency for its *value*, and nothing for its *shape*.

## Implementation status

Landed 2026-08-27, all nine phases of the plan below.

| | state |
|---|---|
| `scalar_leaf_root` / `parse_scalar_leaf` (`raster-core::input`) | **landed.** One hash each way, reusing the existing `pub IndexWidth` encoder; the private `u64_leaf_root` reduced to a call through it |
| `[chain.input]`, plain and indexed (§1) | **landed.** The indexed form flattens to `<family>_0 … _N` at load, so `{ input = "layer_{l}" }` is ordinary substitution and nothing downstream knows families exist |
| Interleaved item list (§2) | **landed** via `toml::Spanned` merge-sort; JSON takes a tagged `items` array, with `stages` kept as the shorthand every existing `chain.json` uses |
| `ChainManifest` / `ChainSpec` split (§4) | **landed.** `ChainSpec` lost its `Deserialize`, so an unexpanded value of the type every consumer indexes positionally is now unconstructible rather than merely discouraged |
| Static expansion (§2) | **landed** in `raster-cli/src/chain/expand.rs` — textual substitution, `start`, `first`, one `{ident-1}`, exports with per-export `entry`, fixed ordering |
| `ChainShape` on `ChainCommitment` (§5) | **landed**, unconditional. Chain-commitment format break; recorded digests move |
| `verify_shape` + `VerifiedCounts` (§6) | **landed.** `expand` is uncallable without counts, and `verify_shape` is the only thing that makes them — "ordering, not trust" is a property of the types |
| Stage-produced counts (§4) | **landed.** The run loop re-expands from the manifest each round and asserts the executed prefix did not move; a `chain-shape` sidecar makes `--stage` work on a dynamic chain |
| `ChainFaultKind::Shape` (§6.1) | **landed** beside `Link`. `ChainFraudEvidence::Shape { repeat_index }` and nothing else; `prove_chain_fraud` takes `Option<Receipt>` |
| Acceptance on `raster-inference` | **landed.** The 35 `prefill_prepare_aux` stages are one repeat block plus one indexed input, and the manifest expands to the **identical 74 stages** |

**Three things the implementation forced, none of them in the design.**

1. **TOML cannot interleave two arrays-of-tables.** `[[chain.stage]]` and `[[chain.repeat]]` reach
   serde as two independent `Vec`s with no ordering between them, and `raster-inference` is
   `stage, stage, repeat, …`. Since `InputBindingSource::Chained` records a producer by index into
   the expanded list, that ordering is consensus-critical. Recovered from source spans — which also
   forces `spec_digest` to be taken over the normalized item list, making §5's "digest the parse,
   not the file" a requirement rather than a preference.

2. **`max` was readable from the record being checked.** `verify_count` correctly takes the bound
   from the manifest, which `spec_digest` pins — but `RepeatResolution.max` was then dead data a
   claimer could set to anything. It is now a checked equality, so the field cannot become a place
   to put a convenient number.

3. **A fraud prover must be able to load a fraudulent chain.** `verify_shape` in the load path
   refuses a bad shape, which is right for a verifier and fatal for `fraud-prove` — it has to
   reconstruct the chain the claimer *asserts* in order to exhibit what is wrong with it. Split
   into a `ShapePolicy`: `Verify` for `audit`, `AsClaimed` for the two fraud commands.

**Expansion is not digest-neutral, and that is the intended reading** — see §5. Two manifests that
expand to the same stages produce identical `StageCheckpoint`s and different chain digests, because
the digest covers the manifest. Both halves are pinned by tests so neither can later be mistaken for
the other.

## Open questions

- **Should the shape record pin the expansion, or just the counts?** §5 records counts and
  re-derives stages. Recording an expansion digest as well would let a verifier detect a
  template/expansion mismatch without re-implementing substitution — at the cost of a
  second thing that can disagree. Slightly more pressing now that §4's `expand` has three
  callers: a bug in it is a bug in the audit that is supposed to catch it.
- **Nested stage-produced counts.** §4 handles them by iterating partition/resolve/expand,
  but a block whose *inner* count comes from a stage inside the outer block is a case the
  structural rule currently forbids outright. Whether that is too strict is unclear
  without a second use case.
- **Is the adapter stage a real cost?** §3 requires a stage-produced count to be the whole
  output of a stage returning `uN`. If planners in practice want to emit a count alongside
  other values, every chain pays one extra stage to project it out. The alternative is a
  payload decoder in `chain_fraud`, which §3 rejects; the question is whether a third option
  exists — e.g. committing the count as its own `output.bin` from a multi-output stage, once
  `program-chain.md` v1's single-output restriction lifts.

**Resolved in the 2026-08-27 revision**, recorded so the reasoning is not re-derived:
`{last}` is gone in favour of named exports (§2); a stage-produced count no longer takes a
`select` path (§3); `manifest_digest` became `spec_digest` over the decoded `ChainSpec` (§5).
