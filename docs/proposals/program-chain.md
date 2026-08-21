# Proposal: `program-chain` — provable multi-program execution

Status: proposed (2026-07-17)
Companion to: [`program-start.md`](./program-start.md), [`program-end.md`](./program-end.md)
(both implemented)
Depends on: [`program-identity.md`](./program-identity.md) (proposed) — supplies the
`program_commitment` this proposal's checkpoints name; supersedes the CFS-hash sketch
below (see the note under Design).

## Problem

`ProgramStart`/`ProgramEnd` made a raster program behave like a function: authorized
input in (manifest commitments → storage at `[]`), authorized output out (storage →
`output.bin`/`output.rindex`/`output_manifest.json`, byte-for-byte the input-manifest
shape). `program-end.md` closed with "programs compose: one program's
`output_manifest.json` + `output.bin` is the next program's `--input-manifest` +
`--input`" — but that claim is untested and, examined closely, unprovable as stated:

1. **No program identity exists anywhere.** The CFS (the program's definition) is a
   private guest input (`env::read::<ControlFlowSchema>()` in
   `guests/transition/src/fraud_proof.rs`); it is never hashed into `TransitionJournal`
   (`crates/raster-core/src/transition.rs:229`) or into the host-side `TraceCommitment`
   (`crates/raster-prover/src/trace.rs`, `{ fingerprint, revealed_items }` only). Two
   different programs, proven with the same transition guest, produce commitment files
   that are mutually indistinguishable by *code* identity — only their trace fingerprint
   and (once added) manifest hash differ. A chain checkpoint that is supposed to say
   "program P mapped input I to output O" currently cannot name P.
2. **The checkpoint names nothing.** A `commit` file is postcard of `{fingerprint,
   revealed_items}` — it doesn't even bind the input manifest that authorized the run.
3. **The journal-committed output hash and the manifest-committed output hash are two
   different hashes of the same value**, and nothing today asserts they agree.
   `ProgramEnd.output_commitment` is `selection_payload_hash(selected_bytes)` — a flat
   `sha256` of the canonical payload (`crates/raster-core/src/input.rs:556`) — while the
   *manifest* entry commitment `write_program_output_artifact` writes is the raster
   **structural root** (`crates/raster-runtime/src/input.rs:2062`,
   `encode_raster_value` → `RasterIndex` root hash). A downstream program authorizes
   against the structural root, not the journal's `selected_hash`. The bridge between
   "the guest proved this output" and "the next program authorized this input" is
   unstated.
4. **The artifact is written by the untrusted user process**, and nothing in
   commit/audit checks it against the trace (the recorder only cross-checks
   `selected_hash` against its own replica — it never touches the on-disk artifact
   bytes).
5. **The output entry name is hardcoded to `"output"`**, while a downstream program
   authorizes each parameter by its own declared name — feeding one program into
   another today requires the consumer to declare a single parameter literally named
   `output`, and hand-authoring the consumer's `input.json` by hand.

None of this is a defect in `ProgramStart`/`ProgramEnd` — they were designed to make one
program's boundary authorized and self-contained. It is the *next* layer, chaining those
boundaries across programs, that has no protocol shape yet.

## Goal

Split one very large execution into a chain of smaller raster programs, where each
program's authorized output is the next program's authorized input, provable at two
levels:

- **Checkpoint level**: the chain's links — `(program identity, input commitment, output
  commitment)` per stage — verify end-to-end by public, cheap checks (no proving
  required to check a link).
- **Intra level**: fraud in any single stage's execution is provable with the existing
  fraud-proof window machinery, unchanged, and the resulting fraud receipt is
  unambiguously attributable to the stage it came from.

This lets raster scale to executions bigger than one program/one trace can hold, by
composing programs the same way `ProgramStart`/`ProgramEnd` already let one program
compose input and output.

Design decisions taken:

- **Program identity is `sha256` of the CFS**, committed into both the journal and the
  host-side trace commitment, so a checkpoint can name the program it attests.
- **Chain links are verified by public hash equality over artifact bytes, not by a new
  ZK proof.** The two output hashes (`ProgramEnd`'s payload hash and the manifest's
  structural root) are both pure functions of `output.bin`; a chain verifier holding the
  bytes recomputes both and checks they match the two sides of the link. This is sound
  and free — it costs nothing beyond data availability of the artifact.
- **Linear chains only** for v1 (single output → single named input per link).
  Multi-output/DAG chains are future work — the manifest format already "leaves room to
  grow" per `program-end.md`.
- **No cross-program ZK aggregation in v1.** Each stage keeps its own independent
  optimistic commit/audit/fraud-proof lifecycle; the chain-level object is a public,
  cheaply-checkable list of stage checkpoints, not a single succinct receipt over the
  whole chain. A chain-aggregation guest is named explicitly as future work.

## Facts the design builds on (verified in code)

- `ProgramEnd.output_commitment == selection_payload_hash(selected_bytes) ==
  sha256(selected_bytes)` (`crates/raster-core/src/input.rs:556`), verified in-guest via
  `verify_selection_witness` (`checks/entrypoint.rs`).
- `write_program_output_artifact` (`crates/raster-runtime/src/input.rs:2111`) writes
  `output.bin` as exactly those canonical payload bytes
  (`encode_raster_value`/`write_raster_files`, same file, lines 2062–2092) — so
  `sha256(output.bin) == ProgramEnd.output_commitment` always holds by construction.
  This is the first half of the bridge, already true today without any change.
- The manifest commitment written alongside it is the **raster structural root**
  (`RasterIndex` root hash from `encode_raster_value`), reconstructible from the same
  `output.bin` bytes via `parse_subtree_root` (`crates/raster-core/src/input.rs:275`,
  currently a private `fn`) — the same routine `verify_selection_proof` uses to walk a
  selection proof from raw bytes. Both link hashes are therefore pure, public functions
  of one file.
- `start_program` (`crates/raster-runtime/src/entry_arguments.rs:91`) authorizes each of
  `main`'s parameters **by declared name**, reading `(encoding, commitment)` from the
  manifest per name (`SourceResolver::manifest_commitment_metadata`) — never from file
  bytes. The combined-root check (`combined_root`, `checks/entrypoint.rs`) folds
  `(name → commitment)` pairs from the CFS-declared names. This means the fixed
  `"output"` key in `output_manifest.json` is a pure naming/wiring problem, not a
  cryptographic one: only the *commitment value* needs to carry from stage N to stage
  N+1, under whatever name stage N+1's parameter uses.
- `TransitionJournal` already threads state across steps via `TransitionState::{Init,
  Next, Finished}`, with `Next` recursively verifying the previous step's receipt
  (`env::verify(transition_image_id, prev_journal_bytes)`,
  `fraud_proof.rs::verify_previous_journal`) and asserting **manifest continuity**
  (`assert_manifest_continuity`: `input.authorization_journal.manifest_commitment ==
  prev_journal.manifest_commitment`). This is the exact recursion pattern a
  program-identity continuity check would mirror — it is proven to work intra-program
  today.
- The authorization guest already turns manifest bytes into
  `AuthorizationJournal { external_inputs_commitments, manifest_commitment =
  sha256(manifest_bytes) }` — the same "hash the bytes, commit the hash" pattern this
  proposal applies to the CFS.
- `TraceCommitment` (`crates/raster-prover/src/trace.rs`) is `{ fingerprint,
  revealed_items }` and `TransitionJournal.init_state.fingerprint` carries the same
  `Fingerprint` value — so a fraud receipt is already matchable to a specific commit
  file by fingerprint equality; adding program/manifest identity to both sides extends
  an existing correspondence rather than inventing one.
- hello-tiles' `main(personal_data: PersonalData, personal_data_bin: PersonalData, seed:
  u64) -> String` already returns a value and emits an output artifact — it is a
  ready-made stage 1 for the end-to-end verification of this proposal without needing a
  new example program.

## Design

> **Superseded by [`program-identity.md`](./program-identity.md).** This proposal
> originally defined program identity as `sha256` of the canonical CFS bytes. That is
> insufficient (the CFS is code-blind — it names tiles but commits no tile code) and
> unsound as stated (the tile replay image id was host-supplied and unbound).
> `program-identity.md` replaces it with `program_commitment = sha256("raster/program/v1"
> || postcard(ProgramDefinition))`, where `ProgramDefinition { manifest, cfs, tiles }`
> bundles the `Raster.toml` interface, the canonical CFS, and the tile image-id registry,
> serialized to a `program.bin` artifact. Everywhere below that says "hash of the CFS" or
> `ControlFlowSchema::commitment()`, read "hash of the `ProgramDefinition` bytes
> (`program.bin`)". The byte-frame / hash-then-decode / `assert_program_continuity`
> mechanics are unchanged — only the *contents* of the committed bytes grow. Identity
> verification (below) accordingly has two modes: **light** (recompute one `sha256` over
> a supplied `program.bin`) and **deep** (rebuild `ProgramDefinition` from source + the
> pinned risc0 toolchain and byte-compare).

### 1. `raster-core`

- Program identity lives on `ProgramDefinition` (`program.rs`), per
  `program-identity.md`: `canonical_bytes()` (the `program.bin` content) and
  `commitment()` (`sha256(domain-prefix || canonical_bytes())`). The CFS is one of its
  three parts; there is no standalone `ControlFlowSchema::commitment()`.
- `input.rs`: promote `parse_subtree_root` to a small public wrapper,
  `pub fn payload_structural_root(bytes: &[u8]) -> Option<Hash32>`, so both the CLI's
  chain verifier and any external verifier can recompute the manifest-side hash from
  `output.bin` without depending on runtime-crate internals.
- `transition.rs`: `TransitionJournal` gains `program_commitment: Vec<u8>` (sibling of
  `manifest_commitment`).

### 2. Transition guest (`crates/raster-prover/guests/transition`)

- `PublicParams::read()` (`fraud_proof.rs`) changes from `env::read::<ControlFlowSchema>()`
  to reading a raw byte frame (the `program.bin` bytes), `sha256`-ing it to produce
  `program_commitment`, then postcard-decoding the `ProgramDefinition` (CFS + tile
  registry + manifest) from those same bytes — mirroring exactly how the authorization
  guest derives `manifest_commitment` from `manifest_bytes`. This means the journal
  commits to the actual bytes the guest verified against, not a host-supplied claim. The
  decoded tile registry is what closes the replay-image-id hole (see
  `program-identity.md`).
- `commit_journal` sets `TransitionJournal.program_commitment`.
- `Next`-step verification gains `assert_program_continuity` (sibling of
  `assert_manifest_continuity`): `input`'s program bytes hash must equal
  `prev_journal.program_commitment`. A fraud receipt therefore names
  `(program_commitment, input_manifest_commitment, init_state.fingerprint)` — enough to
  attribute it to exactly one stage of exactly one chain run.
- Host side (`crates/raster-prover/src/transition.rs`): the CFS write to the guest
  environment changes from a typed `builder.write(&cfs)` to a byte-frame write of the
  `program.bin` bytes `ProgramDefinition::canonical_bytes()` produces, so host and guest
  agree on what was hashed.

### 3. `raster-prover` host (`src/trace.rs`, new `src/chain.rs`)

- `TraceCommitment` gains `program_commitment: Vec<u8>` and
  `input_manifest_commitment: Vec<u8>` (same values the journal now carries;
  `input_manifest_commitment` is the `manifest_commitment` rename from
  `program-identity.md`) — a commit file becomes self-naming: which program, which
  authorized inputs, which trace.
- New `chain.rs`:
  ```rust
  pub struct StageCheckpoint {
      pub name: String,
      pub program_commitment: Vec<u8>,
      pub input_manifest_commitment: Vec<u8>,     // sha256(input_manifest bytes)
      pub input_bindings: BTreeMap<String, InputBindingSource>, // param -> External | Chained { stage }
      pub output_payload_commitment: Vec<u8>,     // sha256(output.bin) == ProgramEnd.output_commitment
      pub output_structural_commitment: Vec<u8>,  // payload_structural_root(output.bin)
      pub trace_commitment: TraceCommitment,
  }

  pub struct ChainCommitment {
      pub stages: Vec<StageCheckpoint>,
  }
  ```
- Link verification (pure, public, no proving): for each `Chained { stage }` binding,
  `input_bindings[param] == stages[stage].output_structural_commitment`; recomputing
  both `sha256(output.bin)` and `payload_structural_root(output.bin)` from the actual
  artifact bytes and checking them against `output_payload_commitment` /
  `output_structural_commitment` catches an artifact that was swapped or corrupted after
  the stage ran (closes the "artifact written by an untrusted process" gap — the chain
  verifier never trusts the file, only recomputes from it).
- Identity verification (per `program-identity.md`): `program_commitment` for stage N
  must equal the identity of the program declared for stage N in `chain.json` — checked
  in **light** mode (`sha256` over that stage's supplied `program.bin`) or **deep** mode
  (reassemble `ProgramDefinition` from source + the pinned toolchain in the stage's
  `Raster.lock`, and byte-compare). This is what stops a chain from silently swapping in
  a different program at some stage.

### 4. `raster-cli`

- **Stage-runner extraction**: `run()` (`commands/run.rs`) is currently hardwired to
  `Project::new(current_dir)`; extract its build/run/artifact-collection core into a
  function parametrized by project path + input paths, reusable by both `cargo raster
  run` (unchanged CLI behavior) and the new `chain` subcommand.
- **`chain.json`** (the pipeline definition):
  ```json
  {
    "stages": [
      { "name": "summarize", "project": "path/to/project-a",
        "inputs": {
          "personal_data": { "external": { "path": "...", "index_path": "...", "commitment": "..." } },
          "seed":          { "external": { "path": "...", "commitment": "..." } }
        } },
      { "name": "expand", "project": "path/to/project-b",
        "inputs": {
          "summary": { "from": "summarize" },
          "seed":    { "external": { "path": "...", "commitment": "..." } }
        } }
    ]
  }
  ```
  `from: <stage>` binds a parameter to that stage's single output (v1: one output per
  stage); `external` binds a parameter the same way a top-level `--input`/
  `--input-manifest` does today.
- **`cargo raster chain run <chain.json>`**: for each stage, in order —
  1. build the stage's CFS and, for every non-terminal stage, assert
     `main_produces_output()` (fail fast before running anything, rather than
     discovering a unit-output stage mid-chain);
  2. synthesize `input.json`/`input_manifest.json` for the stage: `external` bindings
     copy through unchanged; `from` bindings resolve to the previous stage's
     `output.bin`/`output.rindex` and its `output_structural_commitment`, written under
     the consumer parameter's name (the naming remap described in Facts — sound because
     only the commitment value needs to carry over);
  3. run the stage (reusing the extracted runner), `commit` it, compute
     `output_payload_commitment`/`output_structural_commitment` from the produced
     artifact, and assemble its `StageCheckpoint`;
  4. after all stages: write `chain-commitment` (postcard of `ChainCommitment`) and
     print the chain digest.
  Chain run artifacts land under `target/raster/chains/<chain_run_id>/<stage>/`,
  mirroring today's `target/raster/runs/<run_id>/` layout per stage.
- **`cargo raster chain audit <chain.json> <chain-commitment>`**: runs the link +
  identity checks of §3 against the recorded stages, then, for any stage the caller
  wants intra-verified, runs the existing `--audit`/`prove` path unchanged and reports
  the resulting fraud proof (if any) tagged with its stage name.

### 5. Unchanged

`raster-runtime`, `raster-macros`, and the authorization guest need no changes — the
chain composes entirely from the two boundaries `ProgramStart`/`ProgramEnd` already
established. This is the intended payoff of doing those two proposals first.

## Resulting shape

```
Stage "summarize"                          Stage "expand"
ProgramStart []  (external inputs)          ProgramStart []  (from: summarize, + external)
  ...tiles...                                 ...tiles...
ProgramEnd   []  -> output.bin  ─┐            ProgramEnd   []  -> output.bin
                                  │
                    sha256 ──────┤ == ProgramEnd.output_commitment (journal-verified)
                    structural   │ == next stage's manifest entry commitment
                    root ────────┘    (authorization-journal-verified)

ChainCommitment = [
  StageCheckpoint { name: "summarize", program_commitment, input_manifest_commitment,
                     output_payload_commitment, output_structural_commitment, trace_commitment },
  StageCheckpoint { name: "expand",    program_commitment, input_manifest_commitment,
                     input_bindings: { summary: Chained("summarize"), ... }, ... },
]
```

A verifier holding the `chain.json`, the `ChainCommitment`, each stage's program source,
and each stage's `output.bin` can check the whole chain's links and identities without
running a prover. Any stage can additionally be optimistically audited with the
unmodified fraud-proof machinery, and the resulting receipt names exactly the stage it
came from.

## Fraud semantics at the chain level

Three distinguishable failure classes:

1. **Link fraud** — a stage checkpoint's committed output hashes don't match the
   recomputed hashes of its `output.bin`, or don't match the next stage's declared input
   binding. Publicly checkable given the artifact bytes; no proving involved. If the
   bytes are withheld, the chain is **unverifiable**, not proven fraudulent — this is a
   data-availability assumption, stated honestly rather than hidden.
2. **Identity fraud** — a stage checkpoint's `program_commitment` doesn't match the
   hash of the program source declared for that stage in `chain.json`. Publicly checkable
   given the program source, the same way link fraud is checkable given the artifact.
3. **Intra-stage fraud** — the existing `FraudEvidence` window / transition-guest fraud
   proof, unchanged, now stage-attributable via `(program_commitment,
   input_manifest_commitment, init_state.fingerprint)`.

What cannot be attested: a stage that errors or panics publishes no `ProgramEnd` and no
artifact (unchanged `program-end.md` success-only decision) — the stage is simply
unattested, and the chain halts there; nothing downstream of a break has an authorized
input, so nothing after it can be attested either. A mid-trace fraud receipt for a stage
legitimately shows `output_authorization: Pending` (the documented asymmetry in
`program-end.md` §7) — chain link-checking requires each linked stage's *completed,
audited* trace, so this doesn't change.

## Alternatives considered

- **A dedicated "bridge" guest** committing `(sha256(bytes), structural_root(bytes))`
  pairs as a succinct proof of the link, instead of public recomputation: rejected for
  v1 — sound and linear-cost, but it's new proving surface for a fact anyone holding the
  artifact bytes can already check with two hash functions. Left as explicit future work
  for when succinctness (not just soundness) of the link matters, e.g. a verifier that
  cannot afford to hold every stage's artifact.
- **A single ZK chain-aggregation guest** verifying all stage journals + all links into
  one succinct receipt: rejected for v1 as unnecessary complexity — the optimistic
  per-stage model (commit/audit/fraud-proof) is exactly what `ProgramStart`/
  `ProgramEnd` already built and proved out; a chain is a list of independently
  verifiable stages before it needs to be a single receipt. Noted as future work.
- **One giant program using `recur` instead of chaining separate programs**: does not
  solve the actual problem — it has no restart/isolation/memory boundary, which is the
  scaling limit this proposal exists to address.
- **Committing the CFS's full contents (not just its hash) into the journal**: rejected
  — bloats every journal/receipt; a hash plus publicly available program source is
  sufficient, matching how the manifest is already handled (hash committed, bytes public).
- **Making `ProgramEnd` commit the structural root in-guest instead of
  `selected_hash`**: already rejected in `program-end.md` (Merkleizing inside the
  riscv32 guest); this proposal instead treats the two hashes as a public bridge,
  consistent with that prior decision.
- **A Merkle tree over stages for the chain digest** instead of an ordered list:
  overkill for v1's linear chains; revisit if/when chains become DAGs.

## Implementation order

1. `raster-core`: `ProgramDefinition` identity (`program-identity.md`); public
   `payload_structural_root`; `TransitionJournal.program_commitment`.
2. `raster-prover` host: CFS byte-frame write; `TraceCommitment` naming fields.
3. Transition guest: hash-then-decode `PublicParams::read`; `program_commitment` in
   `commit_journal`; `assert_program_continuity` on `Next` (guest rebuild via
   `raster-prover/build.rs`; regenerate fixtures per project convention).
4. `raster-cli`: stage-runner extraction (behavior-preserving refactor of `run()`),
   `ChainCommitment`/`StageCheckpoint` types, `chain run`/`chain audit` subcommands,
   input synthesis, link + identity verification.
5. A second stage project consuming hello-tiles' `String` output, for end-to-end
   verification; extend `crates/raster-cli/tests/recur_cli.rs`-style integration tests
   with a two-stage chain test.

## Verification

- Unit: `sha256(output.bin) == ProgramEnd.output_commitment` and
  `payload_structural_root(output.bin) == output_manifest commitment` on a real
  hello-tiles run (this should already hold today, before any code change — confirming
  it is the first check); CFS commitment determinism (same program → same hash across
  rebuilds); guest test for `assert_program_continuity` rejecting a `Next` step whose
  program bytes hash disagrees with the previous journal.
- End-to-end: a two-stage chain over hello-tiles (`summarize` stage = existing
  hello-tiles binary; `expand` stage = a small second project consuming its `String`
  output) — `chain run` produces a `ChainCommitment` whose links and identities verify;
  `chain audit` passes clean; each stage's individual `--commit`/`--audit` still works
  unmodified.
- Negative: tamper stage-1's `output.bin` after the run (link check fails, publicly, no
  proving needed); bit-flip a mid-stage step via the existing `random_bit_flip.sh`
  workflow (the existing fraud proof still concludes, and its journal's
  `(program_commitment, input_manifest_commitment, fingerprint)` matches the stage checkpoint
  that named it); point a chain stage at a different program binary than the one its
  checkpoint claims (identity check fails); a chain stage whose `main` returns `()` but
  is not the last stage (rejected before execution, per `main_produces_output()` check).

## Out of scope

- DAG / multi-output chains (a stage feeding more than one downstream parameter, or
  downstream stages merging outputs from more than one upstream stage). The
  `StageCheckpoint`/`chain.json` shapes don't block generalizing `from: <stage>` to
  `from: { stage, output }` later; `program-end.md` already left room for named,
  multiple outputs.
- A succinct ZK chain-aggregation guest (single receipt over the whole chain).
- The "bridge" guest alternative (succinct link proof instead of public recomputation).
- Postcard-encoded chain links (mirrors the existing input/output-artifact raster-only
  constraint).
- Cross-machine orchestration or transport for artifacts between stages — this proposal
  assumes data availability of each stage's `output.bin`/`output.rindex` to whoever
  verifies the chain, the same way `program-end.md` assumed it for a single program's
  artifact.
