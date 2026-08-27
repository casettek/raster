# Proposal: `program-manifest` — one `Raster.toml`, a named program artifact

Status: proposed (2026-08-26)
Companion to: [`program-identity.md`](./program-identity.md) (implemented) — defines
`ProgramManifest`, `program.bin`, and the "optional with derived defaults" rule this
proposal reverses; [`program-chain.md`](./program-chain.md) (partly implemented) and
[`chain-stage-execution.md`](./chain-stage-execution.md) (partly implemented) — between them
they introduced the `[chain]` table this proposal has to sit next to.
Precedent: Cargo's `Cargo.toml` / `Cargo.lock` and its `[package]` / `[workspace]` split,
which `program-identity.md` already cites as the model for `Raster.toml` / `Raster.lock`.

## Problem

Four symptoms, one cause: **`Raster.toml` has no owner.** It is a filename that two
unrelated parsers in two unrelated modules both claim, and that no program on disk
actually uses.

**1. The identity-bearing manifest is never authored.** `load_or_synthesize_manifest`
(`crates/raster-cli/src/program.rs:96`) parses `Raster.toml` if it exists and otherwise
calls `synthesize_manifest` (`:119`), which fabricates `name` from `cfs.project` and
`version` from a hardcoded `"0.0.0"`. `program-identity.md` §Manifest slimming called this
"optional with derived defaults … existing programs keep working with zero ceremony."
The result, four weeks on:

```console
$ find . -name Raster.toml -not -path './target/*'
./examples/chain-example/Raster.toml
```

That one file is a **chain** manifest. **Zero** raster programs in this repository author
the document that determines their identity. Every `program_commitment` in the tree —
including the three checked-in `Raster.lock` files — is computed over a name the author
never wrote and the version string `"0.0.0"`.

**2. `Raster.toml` means two different documents.** `RasterToml { program, inputs, output }`
(`crates/raster-cli/src/program.rs:22`) and `RasterTomlDoc { chain: Option<ChainTable> }`
(`crates/raster-cli/src/chain.rs:1500`) are separate structs, in separate modules, with
separate `serde` derives, neither aware of the other. `chain.rs` deliberately reads a
document with **no** `[program]` table and calls the absence "the same package-vs-virtual-
workspace split Cargo draws" (`examples/chain-example/Raster.toml`, header comment) — but
nothing enforces the split, because neither parser can see the other's table. A file with
both tables, or neither, is accepted by whichever parser happens to open it and rejected
by the other, with an error naming a field the author didn't think they were writing.

**3. `program.bin` is named after its category, not its contents.** The artifact is written
to `project.output_dir.join("program.bin")` (`crates/raster-cli/src/program.rs:209`) and read
back from the same constant (`crates/raster-cli/src/chain.rs:1472`), where `output_dir` is
`<root>/target/raster` (`crates/raster-compiler/src/project.rs:29`). Every program in every
chain produces a file with the identical name. `program-identity.md` §External representation
sells the artifact on being **portable and archival** — *"a verifier can be handed
`program.bin` with no source tree and no toolchain"*. Hand that verifier a three-stage chain
and they receive three files called `program.bin`, distinguishable only by the directory they
came from, which is exactly the context the archival claim says they don't need.

**4. A stage's membership in a chain is invisible from the stage.** `StageSpec.project`
(`crates/raster-cli/src/chain.rs:67-73`) is a path string in the chain's file. Nothing in
`examples/chain-example/phase2-aggregate/` records that its `filtered` parameter can only
ever be fed by a chain — so `cargo raster run` in that directory is a legal invocation that
fails at input resolution with a missing-file error, rather than at manifest load with
"this parameter is chain-bound."

The cause is the same in all four: there is no single authored document that says *what
this program is* — its name, its version, its interface, and its relationship to a larger
pipeline. There is a file that could be that document, and it is optional, so it is absent.

## Goal

Make `Raster.toml` the one authored manifest for both kinds of raster project, and name the
identity artifact from it.

1. **One grammar, one parser.** `[program]` and `[chain]` are tables in a single documented
   schema, parsed by a single module. Exactly one of them must be present.
2. **`[program]` is mandatory** for a program project. `synthesize_manifest` stops being a
   silent fallback and becomes what `cargo raster init` writes to disk, once.
3. **The identity artifact is `<program.name>.bin`**, so a directory of program frames is
   self-describing and a misfiled frame is detectable by decoding it.
4. **Chain membership is declared, and inheritable** — `version.chain = true` in the Cargo
   `version.workspace = true` idiom, plus a per-parameter `source = "chain"` policy.

Non-goals, stated up front, because the first is the trap in this design:

- **Per-run input *values* do not enter identity.** `[inputs.<name>]` declares a *type and
  an encoding*; it does not declare which file or which commitment. A program whose input
  file changes is the same program. §3 draws this line and §7 makes it a test.
- **The identity construction does not move.** `program_commitment = sha256("raster/program/v1"
  || postcard(ProgramDefinition))` is unchanged; `ProgramDefinition`'s three fields are
  unchanged. Only the authoring surface and the artifact's filename change.
- **No stage references beyond paths.** `[[chain.stage]] project = "<relative path>"` stays.
  A registry, or version-resolved stage references, is a different proposal.
- **`output.bin` / `output.rindex` keep their names.** `program-end.md` fixed them, they are
  per-run artifacts inside a run-scoped directory, and the ambiguity §3 describes doesn't
  arise for them. Flagged in §Out of scope.

## Facts the design builds on (verified in code)

| Fact | Where |
|---|---|
| `ProgramManifest { name, version, inputs, output }` — `name` and `version` are already inside the identity preimage | `crates/raster-core/src/program.rs:56-68` |
| `ProgramDefinition::assemble` already validates manifest-vs-CFS agreement (input names, output presence) and is the single choke point every frame passes through | `crates/raster-core/src/program.rs:88-101` |
| `Raster.toml` parsing for programs: `RasterToml` / `ProgramSection` / `TomlInterface` | `crates/raster-cli/src/program.rs:22-56` |
| `Raster.toml` parsing for chains: `RasterTomlDoc` / `ChainTable`, plus upward discovery | `crates/raster-cli/src/chain.rs:1500-1528` |
| `program.bin` is a **pure build cache** — every consumer regenerates it when absent, via `reassemble_from_lock` (no toolchain) or `build_program_frame` (with) | `crates/raster-cli/src/program.rs:269`, `crates/raster-cli/src/commands/run.rs:762`; `program-identity.md` §External representation |
| `Raster.lock` is `{ format, program_commitment, tiles, toolchain }` — it records the commitment but **not** the program's name | `crates/raster-cli/src/program.rs:59-88`; `examples/chain-example/phase1-normalize/Raster.lock` |
| `InterfaceDecl { type_path, encoding, schema_hash }` — `schema_hash` is filled after load by `fill_schema_hashes`, so the TOML never carries it | `crates/raster-core/src/program.rs:38-51`; `crates/raster-compiler/src/schema_walk.rs:26` |

The last fact is the one that makes this cheap: because `program.bin` is a cache with no
hard consumers, **renaming it is a rename of a string constant in two places**, not a format
migration.

## Design

### 1. One document, one parser (`crates/raster-cli/src/manifest.rs`, new)

A single module owns the whole `Raster.toml` grammar. `RasterToml` (`program.rs:22`) and
`RasterTomlDoc` (`chain.rs:1500`) both retire into it.

```rust
/// The whole `Raster.toml` grammar. Exactly one of `program` / `chain` is present —
/// the package-vs-virtual-workspace split, now enforced in one place.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RasterToml {
    #[serde(default)] pub program: Option<ProgramSection>,
    #[serde(default)] pub chain:   Option<ChainSection>,
    #[serde(default)] pub inputs:  BTreeMap<String, InputSection>,
    #[serde(default)] pub output:  Option<InterfaceSection>,
    #[serde(default)] pub run:     Option<RunSection>,   // §6, separable
}

pub enum Kind { Program(ProgramProject), Chain(ChainSpec) }

pub fn load(path: &Path) -> Result<Kind>;
pub fn discover(from: &Path) -> Result<(Kind, PathBuf)>;   // walks upward, as chain.rs does today
```

`load` rejects, with the diagnostic named:

- both tables present — *"`Raster.toml` declares both `[program]` and `[chain]`; a project is one or the other"*;
- neither present — *"`Raster.toml` declares neither `[program]` nor `[chain]`"*;
- `[chain]` with `[inputs]` / `[output]` at top level — those belong to a program, or to a
  `[[chain.stage]]`;
- `deny_unknown_fields` catches a typo'd key instead of silently defaulting it.

`chain::resolve_chain` (`chain.rs:1528`) and `program::load_or_synthesize_manifest`
(`program.rs:96`) both become thin callers of `manifest::discover`.

### 2. `[program]` is mandatory

`load_or_synthesize_manifest` splits in two:

```rust
/// Load the authored interface. Errors if `Raster.toml` is absent or has no `[program]`.
pub fn load_manifest(project: &Project, cfs: &ControlFlowSchema) -> Result<ProgramManifest>;

/// Derive a manifest from the CFS + main's AST signature. No longer a load-time
/// fallback — this is what `cargo raster init` serializes to disk, once.
pub fn synthesize_manifest(project: &Project, cfs: &ControlFlowSchema) -> ProgramManifest;
```

`cargo raster init` writes the synthesized manifest as TOML at the project root, filling
`name` from the crate name and leaving `version = "0.1.0"`. It refuses to overwrite an
existing file. This is the whole migration path: run it once per project, edit the version,
commit.

The error when `Raster.toml` is missing names the fix:

```
error: no `Raster.toml` in `examples/chain-example/phase2-aggregate`

  A raster program's identity is a commitment over its authored interface. Without a
  manifest the name and version would be inferred, and `program_commitment` would depend
  on facts you never wrote down.

  Run `cargo raster init` to generate one from `main`'s signature.
```

**Why reverse `program-identity.md`'s decision.** That proposal made the manifest optional
to avoid ceremony for existing programs. The evidence above is that the escape hatch became
the only path: not one program authors the file, so every identity in the tree is computed
over `version = "0.0.0"` and a name derived from `cfs.project`. Identity over inferred data
is the failure mode `program-identity.md` §Soundness holes closed exists to prevent, one
level up. This is a **labelled disagreement** with a prior decision, not a silent
re-litigation: the decision was right for a repo with no programs and is wrong for one with
programs that ship chain commitments.

The cost of reversing it is bounded and shrinking: four projects today, all in-tree
(`examples/hello-tiles`, the three `chain-example` stages). It grows with every project
added. Doing it now costs four files.

### 3. `[inputs]` — the line between interface and binding

This is the load-bearing rule of the proposal.

`Raster.toml` has an **identity core** and a **tooling shell**. Only these fields reach
`ProgramDefinition`:

| Field | Enters `ProgramManifest`? |
|---|---|
| `[program] name`, `[program] version` | **yes** |
| `[inputs.<n>] type`, `[inputs.<n>] encoding` | **yes** (`InterfaceDecl`) |
| `[output] type`, `[output] encoding` | **yes** |
| `[program] chain` (§5 pointer) | no |
| `[inputs.<n>] source` (§5 policy) | no |
| `[run.inputs]` (§6 defaults) | no |

The rule that generates the table: **a field enters identity iff changing it changes what a
verifier must check.** Renaming a program, bumping its version, changing a parameter's type
or encoding — all change what the transition guest validates against. Where the bytes for
that parameter came from on some particular run does not; that is what `input_manifest.json`
carries, per run, and what the authorization guest checks per run.

So `[inputs.<name>]` keeps exactly the shape `program-identity.md` §External representation
specified:

```toml
[inputs.readings]
type = "Measurements"
encoding = "raster"
```

and gains only the non-identity `source` key from §5. A binding — path, index path,
commitment — never appears here. `input.json` (private, local, never hashed) and
`input_manifest.json` (public, per-run) keep their current roles unchanged.

**The trap this avoids.** "Define the inputs in `Raster.toml`" reads naturally as "put the
input files there too". If a path or commitment entered `ProgramManifest`, then
`program_commitment` would move every time an input file changed — and a chain stage's
identity would depend on the previous stage's output, which is circular: the chain's link
check compares stage N+1's *declared input commitment* against stage N's *output commitment*,
and the whole point is that those are checked against each other, not defined in terms of
each other. §6's `[run]` table gets the ergonomics without crossing the line, by living
outside the identity core.

### 4. The identity artifact is `<program.name>.bin`

`write_program_artifacts` (`crates/raster-cli/src/program.rs:201`) writes
`output_dir.join(format!("{}.bin", def.manifest.name))`; `read_program_identity`
(`crates/raster-cli/src/chain.rs:1471`) reads the same. Both derive the filename from the
manifest they already hold, so neither needs a new lookup.

```
examples/chain-example/
  phase1-normalize/target/raster/chain-stage-normalize.bin
  phase2-aggregate/target/raster/chain-stage-aggregate.bin
  phase3-report/target/raster/chain-stage-report.bin
```

Three properties this buys:

1. **Archival handoff works as advertised.** `program-identity.md` §External representation
   promises a verifier can be handed the frame with no source tree. Collect a chain's frames
   into one directory and they are now distinguishable without it. `chain run` writes exactly
   such a directory: `target/raster/chains/<run>/programs/<name>.bin`.
2. **The filename is checkable against the contents.** A loader decodes the frame and asserts
   `def.manifest.name == file_stem`. A frame moved into the wrong project's `target/raster/`
   — the plausible accident, since the cache is gitignored and regenerable — is caught by a
   name mismatch rather than by an identity mismatch three steps later. This is *not* a
   soundness property (the frame's hash was already authoritative); it is a diagnostic that
   turns a confusing failure into a clear one.
3. **`Raster.lock` becomes self-describing.** Format 2 adds `name` and `version`, so the
   checked-in file records which program the commitment is for. Today
   `phase1-normalize/Raster.lock` contains a commitment and a tile called `keep_above` and
   nothing that says what program this is.

**`name` becomes a filename, so `name` needs a charset — enforced in `raster-core`.**

```rust
// crates/raster-core/src/program.rs — called from ProgramDefinition::assemble
fn validate_program_name(name: &str) -> Result<()> {
    // 1..=64 bytes, ASCII alphanumeric plus `-` and `_`, first char alphanumeric.
}
```

This must live in `assemble`, not in the CLI's TOML loader. `assemble` is the single choke
point every frame passes through, including one **decoded from untrusted bytes** by
`ProgramDefinition::decode`. A verifier handed a frame and writing it to
`<manifest.name>.bin` with an unvalidated name is a path-traversal write; a verifier
*reading* `programs/<name>.bin` from a chain handoff has the same exposure. Validating in
the CLI would protect our writer and no one else's.

Because `name` is already inside the identity preimage, adding the constraint can only
reject frames — it cannot change the commitment of any frame that passes.

### 5. Chain membership, by inheritance

Cargo's model, which this follows: the workspace root lists members; a member declares
`[package]` and may inherit fields with `<field>.workspace = true`; the workspace is
discovered by walking upward, and `package.workspace` only disambiguates.

**Direction of ownership — the chain owns the wiring.** `[[chain.stage]] inputs` stays
where it is. A verifier must be able to read the entire pipeline graph from one document
without opening N member manifests; that is the property `program-chain.md` §Resulting shape
rests on ("a verifier holding the `chain.json`, the `ChainCommitment` … can check the whole
chain's links"). Moving `from = "normalize"` into the member would scatter the graph across
the tree and make a chain unreadable without the members present.

What the **member** gains is a *declaration*, not a binding — three keys, none of them
identity-bearing except the inherited `version`:

```toml
# examples/chain-example/phase2-aggregate/Raster.toml
[program]
name = "chain-stage-aggregate"
version.chain = true          # inherit "0.1.0" from the chain manifest — identity-bearing
chain = ".."                  # optional; otherwise discovered by walking upward

[inputs.filtered]
type = "Filtered"
encoding = "raster"
source = "chain"              # this parameter is only bindable by a chain stage

[output]
type = "Stats"
encoding = "raster"
```

- **`chain = "<relative path>"`** — a pointer to the directory holding the `[chain]` manifest.
  Optional: absent, the loader walks upward the way `chain.rs:1528` already does for
  discovery. Present, it disambiguates nested chains and makes membership greppable from the
  member. The loader errors if the named chain's `[[chain.stage]]` list has no stage whose
  `project` resolves to this directory — so the two directions cannot silently disagree.

- **`version.chain = true`** — inherit `[chain] version`. Opt-in per member, and the
  trade-off is real and worth stating: inheriting means **a chain version bump moves every
  member's `program_commitment`**, invalidating every stage's `Raster.lock` at once. That is
  sometimes exactly right (a chain released as one unit, where an old `ChainCommitment` should
  be unambiguously about the old chain) and sometimes wrong (a stage shared between two
  chains — which then cannot inherit, and must pin its own version). Because it is opt-in,
  the author picks; because it is one key, the choice is visible.

- **`source`** ∈ `"external"` (default) | `"chain"` | `"any"` — what may bind this parameter.
  Checked at manifest load, before anything runs:
  - `source = "chain"` and the program is run standalone → *"parameter `filtered` is
    chain-bound (`source = \"chain\"` in `Raster.toml`); run it via `cargo raster chain run`
    from `..`"*, instead of today's missing-file error at input resolution.
  - `source = "external"` and a `[[chain.stage]]` binds it with `from = "..."` → rejected at
    chain load, alongside the existing `from`-ordering validation.

  Not identity-bearing, per §3: the guest checks the parameter's *type and encoding*, and is
  indifferent to which of two legal provenances supplied the bytes. It is a CLI contract,
  so it stays in the tooling shell.

**Interface cross-check, now possible.** `program-identity.md` §The interface as an enforced
contract listed a third enforcement point — *"For chaining: a chain author wires stages by
reading each stage's `Raster.toml` — stage N's `[output]` type/encoding against stage N+1's
target `[inputs.<param>]`, validated when `chain.json` is loaded, before anything runs."*
That has never been implementable, because no stage has a `Raster.toml` to read. With §2 it
becomes a load-time check: for every `from = "<stage>"` binding, the producer's `[output]`
`type` and `encoding` must equal the consumer's `[inputs.<param>]` `type` and `encoding`.
A type mismatch across a chain link is caught before the first stage runs, rather than as a
decode failure in stage N+1.

### 6. `[run]` — default bindings, tooling-only (separable)

The half of "define the inputs in `Raster.toml`" that isn't already implemented: a project
carries its own default input paths, so `cargo raster run` needs no `--input`.

```toml
[run.inputs]
readings  = { path = "measurements.rastered", index_path = "measurements.rindex" }
threshold = { path = "threshold.rastered",    index_path = "threshold.rindex" }
```

Semantics: `[run.inputs]` synthesizes the same structure `input.json` holds today
(`examples/chain-example/phase1-normalize/input.json`), and `--input` overrides it wholesale.
Commitments are **not** written here — they stay in `input_manifest.json`, which is per-run
and public. Outside the identity core, per §3.

This phase is separable: dropping it costs nothing in §§1–5. It is listed last for that
reason.

### 7. Enforcing the identity/tooling partition

The partition in §3 is a rule a future contributor can break by adding one field to the
wrong struct. It gets a test, not a comment:

```rust
#[test]
fn tooling_fields_do_not_move_program_commitment() {
    // Same program, two manifests differing only in `chain`, `source`, and `[run.inputs]`.
    // assemble() both; assert equal commitments.
}
```

plus the converse — that changing `name`, `version`, a `type`, or an `encoding` *does* move it.

## Resulting shape

```
examples/chain-example/
  Raster.toml                       [chain] + [[chain.stage]]  — the wiring, unchanged
  phase1-normalize/
    Raster.toml                     [program] + [inputs] + [output] + [run.inputs]   ← new
    Raster.lock                     format 2: + name, version
    target/raster/
      chain-stage-normalize.bin     ← was program.bin
  phase2-aggregate/
    Raster.toml                     [program] version.chain = true; inputs.filtered.source = "chain"
    ...
```

```toml
# examples/chain-example/phase1-normalize/Raster.toml
[program]
name = "chain-stage-normalize"
version.chain = true

[inputs.readings]
type = "Measurements"
encoding = "raster"

[inputs.threshold]
type = "u64"
encoding = "raster"

[output]
type = "Filtered"
encoding = "raster"

[run.inputs]
readings  = { path = "measurements.rastered", index_path = "measurements.rindex" }
threshold = { path = "threshold.rastered",    index_path = "threshold.rindex" }
```

One grammar; the chain manifest above it is the same file with `[chain]` instead of
`[program]`.

## What this breaks

- **Every existing program's `program_commitment` moves, once.** Not because the construction
  changed, but because an authored `version = "0.1.0"` replaces the synthesized `"0.0.0"`.
  All four in-tree `Raster.lock` files regenerate. Chain commitments recorded before the
  change do not verify against post-change sources.
- **This is the first real exercise of the archival property.** `program-identity.md`
  §External representation claims a preserved frame keeps old commitments checkable "even if
  a future raster version changes how the definition is reassembled from source." A pre-change
  `ChainCommitment` plus the pre-change `program.bin` files still verifies in light mode. If
  that turns out not to hold, the claim was wrong and this proposal found it — which is worth
  knowing independently of the rename.
- **`cargo raster run` in a project with no `Raster.toml` becomes an error.** Deliberate, per
  §2; `cargo raster init` is the one-line fix and the error says so.
- **`--dry-run` / `--no-auth` paths are unaffected.** They skip identity entirely
  (`crates/raster-cli/src/chain.rs:166-183`), so they neither read the frame nor care what
  it is called.

## Alternatives considered

- **Keep `program.bin`, add a sidecar `program.json` naming it.** Two files where one
  self-describing name does, and the sidecar is unhashed, so it can lie. Rejected.
- **`<program_commitment>.bin` — name the artifact by its hash.** Content-addressed, no
  charset question, no collisions. But unreadable, and it inverts the diagnostic: you cannot
  tell which program a file is for without hashing it, which is the problem being solved.
  The commitment is already in `Raster.lock`. Rejected.
- **Keep `Raster.toml` optional, just fix the naming.** Half the problem: the artifact would
  be named from a manifest that is still synthesized, so `<cfs.project>.bin` would look
  authored while being derived. Naming the artifact after the manifest is only an improvement
  if the manifest is authored. The two changes are one change.
- **Move `from = "..."` bindings into member manifests.** The natural reading of "just define
  that it comes from the chain", and rejected in §5: it scatters the pipeline graph across N
  files and breaks the single-document verification `program-chain.md` rests on. `source =
  "chain"` gives the member the declaration without taking the wiring.
- **Put input paths/commitments in `[inputs]`.** §3. Circular for chain stages, and makes a
  program's identity depend on its data.
- **A separate `Chain.toml` for chain manifests.** Would resolve the two-parsers problem by
  giving each document its own filename. But Cargo does not do this — `[workspace]` and
  `[package]` share `Cargo.toml`, and a root manifest routinely carries both — and the
  existing `examples/chain-example/Raster.toml` already committed to the shared-filename
  model. Changing it now would be churn for no property gained; §1 fixes the parser split,
  which is the actual defect.

## Implementation order

1. **`raster-core`**: `validate_program_name` in `ProgramDefinition::assemble`
   (`program.rs:88`). Independently useful, no dependents.
2. **`raster-cli`**: new `manifest.rs` owning the whole grammar; `RasterToml`
   (`program.rs:22`) and `RasterTomlDoc` (`chain.rs:1500`) retire into it; both call sites
   rewired. Behavior-preserving refactor.
3. **Artifact rename**: `<name>.bin` at `program.rs:209` and `chain.rs:1472`; filename/manifest
   cross-check on load; `Raster.lock` format 2 (`+ name`, `+ version`); the chain's
   `programs/` handoff directory.
4. **Mandatory `[program]`**: split `load_or_synthesize_manifest` (`program.rs:96`);
   `cargo raster init`; author the four in-tree manifests; regenerate the locks.
5. **Chain membership**: `chain = "<path>"`, `version.chain = true`, `source` policy, the
   producer-output-vs-consumer-input type check at chain load.
6. **`[run.inputs]`** (§6). Separable — droppable without touching 1–5.
7. **Partition test** (§7) lands with 5.

## Verification

- **Unit** — `validate_program_name` rejects `..`, `/`, `a/b`, empty, 65 bytes, leading `-`;
  accepts the four in-tree names. Grammar: both tables → error; neither → error; `[chain]`
  with top-level `[inputs]` → error; unknown key → error naming the key.
- **Identity partition** (§7) — `chain`, `source`, `[run.inputs]` do not move
  `program_commitment`; `name`, `version`, `type`, `encoding` do.
- **Rename** — `cargo raster build` in each chain-example stage writes `<name>.bin`; a frame
  copied into a sibling stage's `target/raster/` is rejected by the stem check with a name
  mismatch, not an identity mismatch.
- **End-to-end** — `cargo raster chain run` over `examples/chain-example` produces a
  `ChainCommitment` whose links and identities verify, with all three stages authoring a
  manifest; `chain audit` passes clean; `--no-auth` and `--stage` paths unchanged
  (`tests/chain_stage_cli.rs`, all 7 tests still green).
- **Archival regression** — record a `ChainCommitment` and preserve the three
  pre-change `program.bin` frames; after the change, light-mode identity verification against
  those preserved frames still passes. This is `program-identity.md`'s archival claim, tested
  for the first time.
- **Negative** — a chain stage whose `[output] type` disagrees with the consumer's
  `[inputs.<param>] type` is rejected at chain load, before stage 1 runs; a `source = "chain"`
  parameter run standalone errors at manifest load naming the chain.
- **Migration** — `cargo raster init` on a project with no manifest produces a file that,
  when `version` is left at the synthesized value, reproduces the *current*
  `program_commitment` byte-for-byte. Confirms the reversal in §2 is a policy change and not
  a format change.

## Out of scope

- **Renaming `output.bin` / `output.rindex`.** Per-run artifacts in a run-scoped directory;
  `program-end.md` fixed the names and the ambiguity §3 describes does not arise for them.
- **A stage registry or version-resolved stage references.** `project = "<path>"` stays.
- **Deep identity mode** — `program-chain.md`'s unimplemented second verification mode
  (rebuild from source + pinned toolchain and byte-compare). Orthogonal; it consumes whatever
  the frame is called.
- **`[[chain.repeat]]`** — [`chain-repeat.md`](./chain-repeat.md) templates stages within the
  `[chain]` table this proposal only reorganizes around. The grammar in §1 must not preclude
  it: `ChainSection` keeps `#[serde(default)] repeat: Vec<RepeatSection>` reserved.
- **Workspace-level Cargo integration** — teaching `cargo raster` to read `[workspace]` from
  the sibling `Cargo.toml`. Adjacent and tempting; a separate decision.
