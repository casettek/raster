# Proposal: `program-identity` — what a raster program *is*, bindingly

Status: proposed (2026-07-20)
Prerequisite for: [`program-chain.md`](./program-chain.md) (proposed) — supersedes its
`program_commitment = sha256(canonical CFS bytes)` sketch.
Series: [`program-start.md`](./program-start.md), [`program-end.md`](./program-end.md)
(both implemented).

## Problem

`program-chain.md` needs every checkpoint to say "program **P** mapped input **I** to
output **O**". Today raster cannot name **P**. There is no program identity anywhere, and
the obvious candidate — hashing the CFS — is both insufficient and unsound as it stands:

1. **The CFS is code-blind.** `TileDef` is `{ id, tile_type, inputs: usize, outputs:
   usize }` (`crates/raster-core/src/cfs.rs:466`) — a tile's *name* and *arities*, no
   code hash, no image id, no type schema. Two programs with the same shape but
   different tile bodies produce an identical CFS. Hashing the CFS would give them the
   same identity.

2. **The replay image id is host-supplied and unbound.** The transition guest proves a
   tile step via `env::verify(replay_image_id, journal)` where `replay_image_id` comes
   from `TransitionInput` (`crates/raster-core/src/transition.rs:109`; check at
   `guests/transition/src/checks/io.rs:82-99`), populated from the prover's own replay
   result. It is never checked against the tile named by the step's `ExecTarget::Tile`,
   nor against any registry. `record_matches_item`
   (`guests/transition/src/checks/cfs.rs:72-103`) matches a step to its CFS item by
   *coordinates + kind only* — the names carried in the trace and fingerprinted are
   never asserted equal to the CFS item id. A prover can substitute **any** binary whose
   committed output matches the recorded witness.

3. **The replay proof does not bind its input.** `TileReplayJournal` is
   `{ output_bytes, draft_transition }` (`crates/raster-core/src/draft.rs:38`); the
   generated tile guest commits output only (`guest_builder.rs:150`). A replay receipt
   proves "O is *some* output of binary B", not "B(recorded input) = O". A prover who
   finds any `I′` with `B(I′) = O` can attest the pair `(I, O)` even when `B(I) ≠ O`.

4. **The CFS byte order is filesystem-dependent.** `tiles[]`/`sequences[]` follow
   `WalkDir` traversal order over `src/` (`crates/raster-compiler/src/ast.rs:123`,
   unsorted). Per-item *content* is deterministic, but a content hash over the current
   ordering is not reproducible across filesystems/platforms.

None of this is a defect in the earlier boundary work — it is the layer *underneath* it
that was never defined: the program as a stable, verifiable object.

## Goal

Define what a raster program consists of so that it has a **unique, binding,
reproducible identity** — one commitment that names the program's control flow, its tile
code, and its declared interface — and thread that identity through execution so any
attested run (and any chain checkpoint) provably names the program it came from.

Design decisions taken:

- **A raster program is a Rust program, so it gets Cargo-style manifest + lock files.**
  `Raster.toml` (authored: name, version, input/output interface) and `Raster.lock`
  (derived: the identity commitment + tile image ids + toolchain).
- **Two definitions of an execution, kept strictly apart.** A *static* program
  definition (identity — same every run) and the *dynamic* per-run execution values
  (already handled by manifests, the authorization journal, and the trace fingerprint).
  The program identity commits only to the static part.
- **`ProgramDefinition` is a first-class object with a canonical byte form**
  (`program.bin`) whose hash is the identity and whose bytes are exactly what the
  transition guest verifies against — preimage and verification input are the same
  bytes.
- **Close the two replay soundness holes in the same release**, because both change every
  tile image id and that churn is free exactly once — before any identity is durably
  committed anywhere.
- **Program identity excludes protocol identity.** The transition and authorization
  guest image ids are protocol identity (already journal-committed); they are *not* part
  of `program_commitment`, so a protocol upgrade does not change what program you have.

## Two definitions of an execution

Everything in a raster run is either static (true of the program, every run) or dynamic
(true of one run). Program identity is the commitment to the static half.

**Part 1 — the program definition (static, identity).** How the program is *expected* to
execute:

- **the interface** — declared input names/types/encodings and the output type/encoding
  (`Raster.toml`);
- **the control flow** — the CFS (topology, tile/sequence names, arities, dataflow
  bindings, entry-argument names, `produces_output`), derived from source;
- **the code** — the tile image-id registry `TileId → [u8; 32]`, which is what actually
  pins each tile's semantics and types (the CFS carries neither).

**Part 2 — the execution values (dynamic, per-run).** *Which* values flowed:

- input value commitments (`input_manifest.json` → `AuthorizationJournal`);
- the trace **fingerprint** — the concrete data path, including any inline call-site
  argument values (these are dynamic — a call-site argument may be a computed runtime
  expression that no static definition can capture);
- output commitments (`ProgramEnd.output_commitment` + the output artifact).

Model boundary, stated honestly: a literal at a **call site** (`call!(tile, 42)`) is
Part 2 — bound by each run's fingerprint, not by program identity. A constant inside a
tile **body** is Part 1 — baked into that tile's image id. The dividing line is the tile
boundary, which is exactly where code identity is measured.

One attested execution is named by this tuple:

```
( program_commitment,           ← Part 1: which program
  input_manifest_commitment,    ← Part 2: which authorized inputs
  fingerprint,                  ← Part 2: which actual data path
  output_manifest_commitment )  ← Part 2: which authorized output
```

The two sides are realized asymmetrically today, and the naming keeps that honest rather
than implying four parallel journal fields. `program_commitment` (new here) and
`input_manifest_commitment` (the renamed `manifest_commitment`) are literal journal /
`TraceCommitment` fields; the `fingerprint` lives in the journal's transition state; the
output boundary is carried by the `ProgramEnd` step (`OutputAuthorization` + its
`output_commitment`) plus the output artifact, whose document digest is
`output_manifest_commitment`. `program-chain.md`'s `StageCheckpoint` is this same tuple
with the output side expanded into the two commitments a link actually needs
(`output_payload_commitment = sha256(payload)` and `output_structural_commitment`, the
manifest's per-value root): a chain links Part 2 of stage N to Part 2 of stage N+1, while
Part 1 names each stage.

**Boundary-commitment naming.** `input_manifest_commitment` and
`output_manifest_commitment` are one symmetric pair — the digests over the program's
authorized boundary manifests, in and out — and are named to show it. This renames the
existing `TransitionJournal`/`AuthorizationJournal` `manifest_commitment` field to
`input_manifest_commitment`; it keeps its meaning (the digest over the input-manifest
document) and, after the manifest slimming below, the two are computed identically:

- `input_manifest_commitment  = sha256(input_manifest bytes)`
- `output_manifest_commitment = sha256(output_manifest bytes)`

Each boundary has two layers, symmetric on both sides: the **document digest** above, and
a **per-value commitment** — the raster structural root of each value, held *inside* the
manifest (per input) and, on the output side, the value the `ProgramEnd` step commits.
The one honest asymmetry lives at the value layer and is the chain "bridge": the
`ProgramEnd` step commits `sha256(output payload)` (`selection_payload_hash`) while the
manifest's per-value commitment is the *structural root* of that same payload — two hashes
of one value, both recomputable from the payload bytes (see `program-chain.md`).

The explicit `_manifest_` in the boundary names is deliberate: it keeps them clear of the
step-level `ProgramStart.output_commitment()` / `ProgramEnd.output_commitment()`
accessors (named for a *step's* output — where `ProgramStart`'s step-output is actually
the program's input) and of the tile-step `TileReplayJournal.input_commitment` below.
Boundary = `*_manifest_commitment`; step = `*_commitment`.

## External representation & API

Three artifacts, following Cargo's authored-vs-derived split:

| File | Written by | In VCS | Contents |
|---|---|---|---|
| `Raster.toml` | author (optional) | yes | `[program]` name/version; `[inputs.<name>]` type + encoding; `[output]` type + encoding |
| `Raster.lock` | `cargo raster build` | yes | `program_commitment`, per-tile `{ image_id, source_hash }`, toolchain `{ risc0, rust, build_mode }`, format version |
| `target/raster/program.bin` | `cargo raster build` | no | canonical `postcard(ProgramDefinition)` — the identity preimage **and** the guest verification frame |

`Raster.toml` example:

```toml
[program]
name = "hello-tiles"
version = "0.2.0"

[inputs.personal_data]
type = "PersonalData"
encoding = "raster"

[inputs.seed]
type = "u64"
encoding = "raster"

[output]
type = "String"
encoding = "raster"
```

**Why `program.bin` exists (it holds no code).** An identity is a hash, and a hash needs
one agreed byte string as its preimage. `program.bin` is that byte string materialized.
It does triple duty:

1. **Identity preimage** — `program_commitment = sha256("raster/program/v1" || bytes)`;
   recomputable by anyone with plain sha256, no raster tooling.
2. **Guest verification frame** — the transition guest receives *these exact bytes*,
   hashes them itself to derive `program_commitment`, then decodes them to drive its
   checks. Because preimage and verification input are one byte string, "the program
   this receipt is about" and "the program this hash names" cannot diverge.
3. **Portable + archival** — a verifier can be handed `program.bin` with no source tree
   and no toolchain (light mode: one hash). And because identity is defined over bytes,
   a preserved `program.bin` keeps old commitments/journals/checkpoints checkable even
   if a future raster version changes how the definition is reassembled from source.

This is the program-level analog of the value artifact: `output.bin` (payload) +
`output.rindex` (a derived navigation index) made a program's *output value* a handable,
independently-verifiable object; `program.bin` does the same for the *program
definition*. It is payload-only — nobody `select!`s into a program, so it needs no index.
(The trace **fingerprint** is a separate mechanism entirely — it commits the execution
*path*, not the program or the output value.)

`Raster.lock` is **not** the identity — it is a reproducible *claim* of it, the record
that ties `program_commitment` to a source revision over time (Cargo.lock semantics:
derived, deterministic, checked in). Invariant everywhere: `lock.program_commitment ==
sha256("raster/program/v1" || program.bin)`.

**Library API** (`crates/raster-core/src/program.rs`, new):

```rust
pub struct ProgramDefinition {
    pub manifest: ProgramManifest,          // from Raster.toml
    pub cfs: ControlFlowSchema,             // derived from source
    pub tiles: BTreeMap<TileId, [u8; 32]>,  // tile image-id registry
}

impl ProgramDefinition {
    /// Validates: registry keys == cfs tile ids; cfs is canonical (sorted, unique);
    /// manifest inputs == cfs entry_arguments; manifest.output present iff produces_output.
    pub fn assemble(manifest: ProgramManifest, cfs: ControlFlowSchema,
                    tiles: BTreeMap<TileId, [u8; 32]>) -> Result<Self>;
    pub fn canonical_bytes(&self) -> Vec<u8>;   // postcard — the program.bin content
    pub fn commitment(&self) -> [u8; 32];       // sha256(domain-prefix || canonical_bytes)
    pub fn decode(bytes: &[u8]) -> Result<Self>; // guest + file loaders
}
```

## The interface as an enforced contract

`Raster.toml` is not documentation — it is checked against the code so it cannot drift:

- **At build**: declared `[inputs]` names/order must equal `main`'s entry arguments
  (the CFS `entry_arguments`), declared types must match the parameter type paths, and
  `[output]` presence must match `produces_output`. A mismatch is a build error, like a
  signature mismatch.
- **At program start**: the runtime already carries a per-argument
  `schema: fn() -> SchemaNode` on `EntryArgumentSpec`
  (`crates/raster-runtime/src/entry_arguments.rs:38`), so a declared input's structure
  can be checked against the *live Rust type's* schema, not merely its name.
- **For chaining**: a chain author wires stages by reading each stage's `Raster.toml` —
  stage N's `[output]` type/encoding against stage N+1's target `[inputs.<param>]` —
  validated when `chain.json` is loaded, before anything runs.

`Raster.toml` is **optional with derived defaults**: absent, `cargo raster build`
synthesizes the manifest from the crate name + `main`'s signature (existing programs keep
working with zero ceremony); present, it is enforced. Either way the effective manifest
is embedded in `ProgramDefinition`, so identity is uniform. The manifest's name/version
supersede the CFS's current `project` and static `version` metadata fields.

## Manifest slimming

With encoding/type declared statically in `Raster.toml`, the per-run manifests carry only
what is genuinely per-run — the value commitments:

- `input_manifest.json` becomes `{ "<name>": "<commitment-hex>", ... }`. The run refuses
  to start if the manifest keys do not match the declared inputs; encoding and structure
  come from `Raster.toml`.
- `output_manifest.json` becomes `{ "output": "<commitment>" }`; its `sha256` is the
  `output_manifest_commitment` above. The payload files keep the `output.bin`/
  `output.rindex` names `program-end.md` established.
- `input.json` (private, local: file paths) is unchanged — it is never published,
  hashed, or committed.

This touches the authorization guest's manifest parsing (`parse_external_input_commitments`
today expects the full `{type, commitment}` entry,
`guests/authorization/src/main.rs:23-35`), so it lands with the other guest-affecting
changes.

## Soundness holes closed

Problem #1 (the CFS is code-blind) is closed by construction: the tile image-id registry
inside `ProgramDefinition` (Part 1) is the tile code identity the CFS lacked. The
remaining three are closed as follows.

1. **Unbound replay image id (Problem #2).** Delete `TransitionInput.replay_image_id`
   entirely — do not keep-and-assert it (that is a redundant second source of truth). The
   guest resolves the expected image id from the registry inside the (identity-committed)
   `ProgramDefinition`, keyed by the step's coordinates → CFS item tile id. **Recur
   iterations resolve via the recur-site coordinates** (`cfs.rs` recur-iteration mapping)
   — the easy-to-miss path, called out explicitly. A tile step whose id is absent from
   the registry is rejected. Additionally, `record_matches_item` asserts
   `ExecTarget`/sequence names equal their CFS item ids, promoting the
   fingerprint-committed names from decorative to bound.

2. **Unbound replay input (Problem #3).** The tile guest template commits `sha256(input)`
   alongside output; `TileReplayJournal` gains `input_commitment: [u8; 32]`; the
   transition guest asserts it equals the hash of the recorded input witness (same place
   `output_bytes` is compared, `io.rs:93-98`). This changes every tile image id — done
   now, once, while no identities exist to migrate. (This is a *tile-step* digest; it does
   not collide with the program-boundary `input_manifest_commitment`, which carries the
   explicit `_manifest_`.)

3. **CFS ordering (Problem #4).** `CfsBuilder` always sorts `tiles[]`/`sequences[]` by id
   and asserts id uniqueness as its final step — one canonicalization point every
   consumer inherits. `SequenceDef.items` order is **semantic** (coordinates index into
   it) and is never sorted.

4. **Stale artifact cache** (found alongside). The risc0 artifact cache keys on
   `source_hash` alone (`crates/raster-backend-risc0/src/risc0.rs:120-149`), so a
   template or dependency change can silently serve a stale image id into `Raster.lock`.
   Re-key on `(source_hash, template_hash, toolchain)` or recompute `compute_image_id`
   on reuse.

## Identity propagation through an execution

The commitment is computed once and then carried by an unbroken chain of custody: at
every hop it is either recomputed from bytes or asserted equal to the previous hop —
never copied on trust.

1. **Build.** `Raster.toml` + source → CFS; tile builds → image-id registry; assembled
   into `ProgramDefinition` → `program.bin`. Its hash becomes `program_commitment`,
   recorded in `Raster.lock`.

2. **Run / commit.** The CLI *reassembles* the definition from source + lock and requires
   the hash to equal `Raster.lock`'s (drift check — a stale lock fails fast with "run
   `cargo raster build`"). `TraceCommitment` gains `program_commitment` and
   `input_manifest_commitment`, making a commit file a self-naming checkpoint.

3. **Proving, inside the guest** (the load-bearing hop). The host feeds the `program.bin`
   bytes as the guest frame; the guest **hashes the bytes itself before decoding**, so
   `journal.program_commitment` is guest-derived, never a host claim. On a window's first
   step (`Init`) the journal's `program_commitment` is set from the frame; on every
   `Next` step, a new `assert_program_continuity` requires the current frame's hash to
   equal `prev_journal.program_commitment` — the exact sibling of the existing
   `assert_manifest_continuity` (`fraud_proof.rs:169-174`), so identity is invariant
   along the recursive journal chain. At every tile step, the registry image id (Part 1)
   gates the `env::verify` of the replay receipt. The final fraud receipt names
   `(program_commitment, input_manifest_commitment, init fingerprint)` — attributable to
   exactly one stage of one chain.

4. **Chain.** `StageCheckpoint.program_commitment` is copied from the lock/commit and
   checked in one of two modes — **light** (one sha256 of `program.bin`; no source, no
   toolchain) or **deep** (rebuild from source + pinned toolchain, byte-compare the
   re-derived `program.bin`).

So Part 1 flows `Raster.toml` + source → `program.bin` → `Raster.lock` → commit file →
every journal → fraud receipt → chain checkpoint, with the guest as the point where it
stops being a host claim and becomes a proven fact.

## Relation to the AuthorizationGuest

The AuthorizationGuest is the **Part 2 identity machine, and it stays deliberately
program-blind** — a clean separation of concerns:

- It takes the per-run `input_manifest.json` bytes and produces `AuthorizationJournal {
  external_inputs_commitments, input_manifest_commitment = sha256(manifest_bytes) }`
  (`guests/authorization/src/main.rs:37-46`; `manifest_commitment` renamed per the
  boundary-naming pair above) — the identity of *the values used in this run*. The two
  layers sit side by side here: `external_inputs_commitments` (per-input structural roots,
  the value layer) and `input_manifest_commitment` (the document digest). It never sees
  `ProgramDefinition`; it neither knows nor cares which program will consume the inputs.
- Its own image id (`authorization_image_id`) is **protocol identity** — already
  committed in the transition journal and verified in-guest (`checks/io.rs:44-55`). It
  stays out of `program_commitment`, exactly as `transition_image_id` does: a protocol
  upgrade must not change what program you have.
- **The two identities meet in exactly one place: the transition guest's `ProgramStart`
  check.** `combined_root(names, authorization_journal)` folds the entry-argument
  *names* from the identity-committed CFS (Part 1: which inputs the program declares)
  with the *commitments* from the AuthorizationJournal (Part 2: which values were
  authorized), and requires the result to equal `ProgramStart.output_commitment`
  (`guests/transition/src/checks/entrypoint.rs:49-64, 102`). That single equality is the
  provable junction "**this program** ran on **these values**."
- Manifest slimming changes only the guest's manifest *parsing*; this junction is
  untouched — the static encoding/type facts simply move from the per-run manifest into
  the identity side (`Raster.toml` → `ProgramDefinition.manifest`), where static facts
  belong.

Honest caveat: tile image ids embed the risc0 toolchain and the full dependency graph
(including the raster runtime itself), so program identity is
`source ⊗ toolchain ⊗ deps`, not source alone — the same source under two raster versions
is two program identities. Local builds are dev-grade (machine-local ids, sufficient for
self-audit on one machine); third-party re-derivation of identity from source requires
risc0 docker deterministic builds + pinned deps, and `Raster.lock` records
toolchain/build_mode so a mismatch is *detectable* rather than silent.

## Resulting shape

```
Raster.toml  ─┐
source ──CfsBuilder──> CFS ──┐
tiles ──risc0 build──> image ids ──┴──> ProgramDefinition
                                              │ postcard
                                              ▼
                                        program.bin  ──sha256(domain‖·)──> program_commitment
                                              │                                   │
                          ┌───────────────────┼──────────────────┐                │
                          ▼                    ▼                  ▼                ▼
                    Raster.lock         guest frame         (portable)      committed in:
                 (claim + toolchain)  (hash-then-decode)   (light verify)   TransitionJournal
                                                                            TraceCommitment
                                                                            chain checkpoints
```

## Alternatives considered

- **`image_id` field inside `TileDef`** (instead of a separate registry): rejected — it
  makes the CFS un-derivable without the risc0 toolchain (native backend, `cargo raster
  cfs`, static analysis all need a plain syn-parse), and forces a two-phase
  `Option<ImageId>` mutation of a struct meant to be canonical.
- **Put the CFS in `Raster.toml`**: rejected — the CFS is *derived* from source; an
  authored copy is a second source of truth that drifts on every edit and turns code
  changes into manifest merge conflicts. Cargo declares deps in `Cargo.toml` but keeps
  the derived graph in `Cargo.lock`; same split here.
- **Registry as a separate host input to the guest** (not inside the committed bytes):
  rejected — a registry the guest reads but does not commit to is host-supplied, i.e.
  Problem #2 reopened.
- **Keep `replay_image_id` and assert it equals the registry value**: rejected — a
  redundant second source of truth; deleting the field removes the attack surface.
- **Strip `version`/`project`/`encoding` from the identity preimage**: rejected —
  hash-then-decode means the committed bytes *are* the decoded bytes; dropping fields
  from the preimage would require a shadow serialization, defeating the pattern.
- **Per-run value commitments in `Raster.toml`**: rejected — that is Part 2; it would
  make identity change with every input.
- **Defer the replay input commitment**: rejected — it is a live soundness hole (range
  membership vs `B(I) = O`), and the image-id churn it causes is free only before
  identities are durably committed.
- **`crates/raster-guest-ids` as the registry home**: rejected — per-project tile image
  ids are known only after building *that* project and cannot live in a workspace crate.
  Its plausible future role is embedding *protocol* guest ids as constants; out of scope
  here.

## Implementation order

1. **`CfsBuilder` canonicalization** (sort + uniqueness) — with a determinism test over
   shuffled file-discovery order. Isolated, unblocks a stable preimage.
2. **All image-id-affecting guest changes together, once**: tile guest template commits
   `sha256(input)`; artifact-cache re-keying; reproducibility fixes (path-independence,
   pinned guest deps, recorded toolchain).
3. **`raster-core`**: `program.rs` (`ProgramDefinition`, canonical bytes, commitment),
   `ProgramManifest` + `Raster.toml` parsing; `TileReplayJournal.input_commitment`;
   `TransitionJournal.program_commitment`; rename `manifest_commitment` →
   `input_manifest_commitment` in `TransitionJournal`/`AuthorizationJournal`; drop
   `TransitionInput.replay_image_id`.
4. **Transition guest**: frame hash-then-decode; registry-driven `env::verify` (incl.
   recur-site resolution); name assertions in `record_matches_item`; replay
   `input_commitment` check; `assert_program_continuity`. Rebuild guest, regenerate
   fixtures per convention.
5. **Authorization guest**: slimmed manifest parsing only.
6. **`raster-prover` host**: write the `program.bin` byte frame; cross-check freshly
   compiled `ReplayResult.image_id` against `Raster.lock` before proving (fast host-side
   failure).
7. **`raster-cli`**: `cargo raster build` emits `program.bin` + `Raster.lock`;
   `run`/`--commit`/`--audit` reassemble and verify vs the lock; new `cargo raster
   program [--verify]` inspection command; enforce the `Raster.toml` contract.
8. **`program-chain.md` delta**: point its `program_commitment` definition and §1/§2 at
   `ProgramDefinition` bytes; document the light/deep identity-check modes.

## Verification

- **Determinism**: the same program discovered in a shuffled file order yields an
  identical `program_commitment`.
- **Guest negatives** (regenerate fixtures, then): a tile step given a
  wrong-but-output-matching image id → reject; a tile absent from the registry → reject;
  `ExecTarget` name ≠ CFS item id → reject; `TileReplayJournal.input_commitment` ≠
  recorded input hash → reject.
- **Lock**: tampering an image id in `Raster.lock` makes the prover refuse *before*
  proving; editing source without rebuilding makes `run` refuse with a stale-lock error.
- **Contract**: a `Raster.toml` input list disagreeing with `main`'s signature fails the
  build; a matching one round-trips.
- **Reproducibility**: two docker-mode builds from different checkout paths produce equal
  tile image ids and an equal `program_commitment`.
- **End-to-end** (hello-tiles): `cargo raster build` → `program.bin` + `Raster.lock`;
  `cargo raster run --commit` produces a checkpoint carrying `program_commitment`;
  `cargo raster program --verify` recomputes it from source and matches.

## Out of scope

- Protocol guest ids collected in `crates/raster-guest-ids` (transition/authorization
  identity is already journal-committed).
- Domain separation on the *manifest* commitment (still `sha256(manifest_bytes)`,
  unprefixed) — retrofitting a prefix there is a separate change.
- Full type schemas in the CFS (arities only; the tile image id is what pins types).
- The chain machinery itself (`program-chain.md`).
- Non-docker third-party reproducibility guarantees.
