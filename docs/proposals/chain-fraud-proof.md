# Proposal: `chain-fraud-proof` — from per-stage fraud proofs to a whole-chain fraud proof

Status: IMPLEMENTED (2026-07-26; proposed 2026-07-22, revised after security
review; commitment binding reworked to a compact header + slice proof so the
guest never ingests the trace-sized `commit.bin`)
Companion to: [`program-chain.md`](./program-chain.md),
[`program-identity.md`](./program-identity.md), [`program-end.md`](./program-end.md),
[`program-start.md`](./program-start.md)
Depends on: `program-chain.md` (stage checkpoints), `program-identity.md`
(`program_commitment`).

## Problem

`program-chain.md` gave the chain two verification levels — **checkpoint** (public
link/identity hash checks) and **intra** (each stage keeps the unchanged single-program
optimistic fraud-proof lifecycle) — and explicitly deferred the join between them:

> No cross-program ZK aggregation in v1. … A chain-aggregation guest is named explicitly
> as future work.

Today that gap has two concrete shapes:

1. **`chain audit` never checks intra-stage *execution* fraud.** It verifies each stage's
   program identity, its `output.bin` hashes, and that every `from` input commits to the
   producer's output root — all public, all cheap. But a stage that *executed its own
   committed trace incorrectly* (the classic single-program fraud) is invisible to the
   chain layer: catching it requires replaying the stage against its `commit.bin`
   (`TraceCommitment`), which is the single-program fraud path the chain never invokes.
2. **A single-stage fraud proof is not tied to the chain.** Even once a stage's fraud is
   proven — a transition fraud receipt whose journal reaches `TransitionState::Finished`
   — that receipt says *"program P's committed trace T diverges"*, not *"chain D is
   fraudulent, and stage i is to blame."* There is no object a verifier (or a settlement
   contract) can hold that condemns the whole pipeline's result and localizes the fault.

And the review of the transition guest surfaced a third, which any chain aggregation
would inherit if left open:

3. **A fraud receipt is not even tied to its own `commit.bin`.** The fingerprint the
   guest proves divergence against is `InitTransition.fingerprint` — a *window slice*
   that the **host** extracts from the committed fingerprint
   (`crates/raster-prover/src/trace.rs:757-772`) and writes into the guest
   (`crates/raster-prover/src/transition.rs:172-178`). Nothing in-guest verifies that
   this slice actually occurs in the stage's `TraceCommitment` at the window's offset.
   A malicious challenger can therefore fabricate a "committed" window fingerprint that
   appears nowhere in `commit.bin`, prove a divergence against *that*, and obtain a
   genuine-looking `Finished` receipt — framing an honest stage. Today this is survivable
   only because the receipt's consumer is the same host that produced it; the moment the
   receipt becomes a standalone object (this proposal's whole point), the gap is fatal.

## Goal

A three-layer fraud model with **one uniform output — a chain fraud proof**: a single
succinct receipt whose journal names `(chain_digest, faulty_stage, program_commitment,
fault_kind)`, produced by a new guest that *verifies a single-stage fraud receipt and
binds it to a chain checkpoint* — exactly as the transition guest already verifies a
previous journal and binds it by manifest/program continuity.

| Layer | Name | Status | Catches | Cost |
|-------|------|--------|---------|------|
| 0 | Checkpoint | exists (`chain audit`) | link / identity fraud | public hashes |
| 1 | Intra-stage execution | exists for one program | a stage's committed trace diverges | replay + 1 transition fraud receipt |
| 2 | **Chain aggregation** | **this proposal** | any of the above, attributed to a stage, as one receipt | 1–2 `env::verify` in a new guest |

## Threat model

Both sides are untrusted, and the design must be sound against each:

- **A dishonest prover** runs the chain and publishes a `ChainCommitment` whose stages
  misexecuted, mislinked, or misdeclared. The fraud proof must be *complete*: every such
  fault must be provable by an honest challenger holding the public artifacts.
- **A dishonest challenger** tries to condemn an honest chain. The fraud proof must be
  *sound*: no `ChainFraudJournal` can exist for a chain whose named stage is honest.
  In particular the aggregation guest never trusts a host claim — not "stage i is
  faulty", not "this window fingerprint came from `commit.bin`", and not "this image id
  is the transition guest".
- **The relying party** (a verifier or settlement contract) holds the `ChainCommitment`
  and the receipt, pins the `chain_fraud` guest's image id out-of-band, and performs
  only public equality checks. Trust in every inner guest is threaded through the
  journal (see "image-id pinning" below), never assumed.

## Facts the design builds on (verified in code)

- A transition **fraud receipt** is a recursion of transition-guest receipts culminating
  in `TransitionState::Finished`, reached exactly when the accumulated fingerprint
  diverges from the committed window fingerprint at the final window index
  (`crates/raster-prover/guests/transition/src/fraud_proof.rs:421-446`). The fraud *is*
  that divergence.
- **The committed window fingerprint is host-supplied.** The guest compares against
  `window_context.init_state.fingerprint` (`.../transition/src/main.rs:50-53`), which the
  host built by slicing the committed fingerprint over the fraud window
  (`crates/raster-prover/src/trace.rs:757-772`). The full `TraceCommitment` — fingerprint
  plus `revealed_items` — is **not** a guest input today; binding it is new work (problem
  3 above), not a pass-through.
- **The window's offset is derivable in-guest.** `init_frontier` is the trace-tree
  frontier *before* the first window item; the tree holds the seed plus one leaf per
  prior step, so `init_frontier.position` equals the window's start index `s`, and the
  committed slice for a window of length `w` is `fingerprint.get_range(s, s + w)` — the
  same range the host extracts (`trace.rs:760-772`). The slice check therefore needs no
  new host-claimed offset.
- **Composition is already `env::verify`.** A `Next` step verifies the previous journal
  via `env::verify(transition_image_id, prev_journal)` and binds continuity —
  `assert_program_continuity` (program_commitment) and `assert_manifest_continuity`
  (input_manifest_commitment) (`fraud_proof.rs:182,208,219`). The aggregation guest
  reuses this exact primitive one level up.
- The transition journal already names most of the attribution triple:
  `program_commitment` (`crates/raster-core/src/transition.rs`),
  `input_manifest_commitment`, and `init_state`. The chain's `StageCheckpoint` stores
  `program_commitment`, `input_manifest_commitment`, `input_bindings`, both output
  commitments, and (as of this proposal) `trace_commitment_digest`
  (`crates/raster-core/src/chain.rs`), and `ChainCommitment::digest` folds them all.
- **The authorization guest already parses a manifest in-proof.** It commits
  `input_manifest_commitment = sha256(manifest bytes)` alongside the parsed
  `external_inputs_commitments: param -> commitment` map
  (`crates/raster-prover/guests/authorization/src/main.rs:44-45`). The `Link` variant
  below reuses it via `env::verify` instead of re-implementing JSON parsing.
- **Host-side detection already exists.** `TraceVerifier::verify` returns
  `VerificationResult::Fraud(FraudEvidence)` with the offending window
  (`crates/raster-prover/src/trace.rs:685,730`); the challenger path in the chain simply
  runs it per stage, seeded from each stage's `commit.bin`.

## Design

Four modules, each with one job:

1. `raster-core` — the shared journal types (guest/host boundary).
2. Transition guest — bind the window to the `commit.bin` it audits (closes problem 3;
   a small, local change at `Init`).
3. `guests/chain_fraud` — verify one stage receipt and attribute it to one chain
   checkpoint (closes problems 1–2; equality checks plus `env::verify`, no new crypto).
4. Host + CLI — detection and driving; no protocol logic.

### 1. `raster-core` — self-attributing journals

A fraud receipt must name *which committed trace* it refutes, so a downstream verifier
can bind it to a checkpoint without trusting the host.

**The identity is a constant-size header, not the file.** A full `commit.bin` scales
with the trace — the packed fingerprint spans every step, and the revealed window is a
window of full step records — so neither guest may ever ingest it. Instead:

- `TraceCommitmentHeader { bits_packer, fingerprint_len, fingerprint_root,
  revealed_items_commitment }` stands for the commitment: `fingerprint_root` is a
  Merkle root over the fingerprint's packed `u64` blocks (leaf `i` =
  `sha256(bits[i].to_le_bytes())`, combined exactly like the trace tree), and
  `revealed_items_commitment = sha256(postcard(revealed_items))`. The identity is
  `header.digest() = sha256(domain || postcard(header))`. `commit.bin` on disk is
  unchanged — the header is derived from it on demand
  (`TraceCommitmentExt::header`).
- `FingerprintSliceWitness` — the ~2–3 proven blocks (with per-block Merkle paths, the
  same `position`-bit path shape every other guest witness uses) covering a fraud
  window's item range: O(window) data regardless of trace length.
- `TransitionJournal` gains `refuted_trace_commitment: Vec<u8>` = `header.digest()`.
  **Not optional, and not fraud-only**: it is established when the window *opens* (see
  §2) and inherited across `Next` steps exactly like `program_commitment` — every
  window audits a specific commitment, honest or not, so the name belongs on every
  journal. (This resolves the previous draft's open question: the field is bound at
  `Init`, where the slice check must happen anyway, not attached at `Finished`.)
- The checkpoint types (`StageCheckpoint`, `ChainCommitment`, `InputBindingSource`)
  move from `raster-cli` into `raster_core::chain` (the chain-fraud guest decodes
  them), and `StageCheckpoint` carries `trace_commitment_digest: Vec<u8>`
  (= `header.digest()`) instead of embedding the whole `TraceCommitment` — the
  `ChainCommitment` stays a few hashes per stage regardless of trace length, and
  `chain audit` gains the check that `commit.bin` on disk matches the digest (pinning
  the artifact to the checkpoint, which the old embedding did implicitly).
- New shared types (in `raster-core` so both the guest and the host use them):

  ```rust
  pub struct ChainFraudJournal {
      pub chain_commitment_digest: [u8; 32], // which chain (== ChainCommitment::digest)
      pub faulty_stage: u32,
      pub stage_program_commitment: Vec<u8>, // == ChainCommitment.stages[i].program_commitment
      pub fault: ChainFaultKind,
      /// Image ids of the inner guests this receipt verified, committed so the
      /// relying party can pin the whole trust chain (see "image-id pinning").
      pub transition_image_id: Vec<u8>,      // Execution faults
      pub authorization_image_id: Vec<u8>,   // Link faults (empty otherwise)
  }

  pub enum ChainFaultKind {
      Execution, // the stage's committed trace diverges (a layer-1 receipt)
      Link,      // a `from` parameter's committed input ≠ producer's output root
  }
  ```

  The previous draft's `Identity` variant and `chain_fraud_image_id` field are dropped —
  see "What was removed, and why" below.

### 2. Transition guest — bind the window to the commitment it refutes

At `Init` (window open), the host writes two more inputs: the commitment's
`TraceCommitmentHeader` and the `FingerprintSliceWitness` covering this window — never
the trace-sized `commit.bin` itself. The guest
(`fraud_proof.rs::assert_window_is_commitment_slice`):

1. Asserts `init_state.fingerprint.bits_packer == header.bits_packer` and, with
   `s = init_frontier.position` (guest-derived — the frontier holds the seed plus one
   leaf per pre-window step, so its position *is* the window's start; never
   host-claimed) and `w = init_state.fingerprint.len()`:
   `s + w <= header.fingerprint_len`.
2. Derives the covering packed-block range `[b₀, b₁]` from `(s, w, bits_per_item)`
   itself, and rejects a witness whose block count or per-block positions differ; each
   block is then proven against `header.fingerprint_root` at its derived index.
3. Asserts each of the `w` window fingerprint values equals the value extracted from
   the proven blocks at bit offset `(s+i)·bpi − b₀·64`
   (`BitPacker::try_get_at_bit_offset`).
4. `refuted_trace_commitment = header.digest()`, threaded through `commit_journal`;
   `Next` steps inherit it from the (recursively verified) previous journal, exactly
   as `assert_program_continuity` carries `program_commitment`.

This turns the host-supplied window fingerprint from a claim into a checked fact: a
`Finished` journal now proves divergence from *the committed fingerprint of the named
commitment, at the offset the window's own frontier fixes* — no challenger-chosen
fingerprint can reach `Finished` under an honest commitment's name. The divergence
logic (`finalize`) is untouched, and the Init-step cost is O(window): a few block
hashes and Merkle paths plus one ~100-byte header hash, independent of trace length.

### 3. New guest `guests/chain_fraud` — the aggregation guest

**Reads:** the `ChainCommitment` bytes, the claimed `faulty_stage`, the fault kind, and
per-kind evidence: for `Execution` the layer-1 fraud journal + `transition_image_id`;
for `Link` the offending parameter name, an `AuthorizationJournal` +
`authorization_image_id`.

**Does:**

1. `chain_commitment_digest = sha256(postcard(ChainCommitment))`, recomputed in-guest
   from the exact bytes it then reads checkpoints from — the host cannot name one chain
   and attribute against another.
2. For `Execution`, with `stage = stages[faulty_stage]`:
   - `env::verify(transition_image_id, fraud_journal_bytes)` — verify the stage fraud
     receipt (composition, same primitive as a `Next` step) — and assert
     `fraud_journal.transition_image_id == transition_image_id` (the id the receipt's
     own recursion was checked against, `fraud_proof.rs:190-193`).
   - assert `fraud_journal.current_state == Finished` — it is a *fraud* receipt, not an
     honest window.
   - **Attribution**, all against the checkpoint:
     - `fraud_journal.program_commitment        == stage.program_commitment`
     - `fraud_journal.input_manifest_commitment == stage.input_manifest_commitment`
     - `fraud_journal.refuted_trace_commitment  == stage.trace_commitment_digest`

     This proves the receipt refutes *exactly this chain stage's committed trace* — not
     a lookalike from another run (§2 made the third equality meaningful: the journal's
     hash is now guest-derived and slice-checked, not host-asserted).
3. For `Link`, with consumer `stage = stages[faulty_stage]` and parameter `p`:
   - assert `stage.input_bindings[p] == Chained { stage: j }` for some `j < faulty_stage`;
   - `env::verify(authorization_image_id, authorization_journal)` and assert
     `authorization_journal.input_manifest_commitment == stage.input_manifest_commitment`
     — the parsed map provably corresponds to the manifest this checkpoint committed,
     with no in-guest JSON parsing;
   - assert `authorization_journal.external_inputs_commitments[p]
     != stages[j].output_structural_commitment` — the committed inconsistency *is* the
     fraud.
4. `env::commit(ChainFraudJournal { … })`.

**Result:** a receipt that says *"chain D is fraudulent; stage i (program P_i) is to
blame,"* verifiable by anyone holding the receipt + `ChainCommitment`, and recursively
verifiable / Groth16-wrappable for on-chain settlement (the "smart contract parameters"
the README anticipates).

**Image-id pinning.** `env::verify(id, journal)` proves "*some* guest with image id
`id` committed `journal`" — so an id taken from an untrusted input proves nothing by
itself: a challenger could compile their own guest that commits an arbitrary `Finished`
journal naming its own image id, and self-verify it. Trust must enter from outside and
be threaded through the journals:

- the relying party pins the `chain_fraud` image id out-of-band (this is irreducible —
  a receipt cannot vouch for its own verifier, which is why the previous draft's
  self-referential `chain_fraud_image_id` field is gone);
- the relying party checks the committed `transition_image_id` /
  `authorization_image_id` against the known-good ids;
- the `chain_fraud` guest guarantees those committed ids are the ones it actually
  verified with, and the transition guest's own recursion already holds its id constant
  across the window (`fraud_proof.rs:190-193`).

Pinning could instead be baked into the `chain_fraud` binary as constants; committing
the ids keeps the guest reusable across transition-guest versions at zero soundness
cost, since the relying party checks them either way.

### 4. `raster-prover` host — drive detection and proving

- `chain::detect_execution_fraud(chain_dir, spec)`: for each stage, re-run the stage
  binary (deterministic native replay) to get the honest trace, build a `TraceVerifier`
  from the stage's `commit.bin`, and `verify`. The first stage returning `Fraud(evidence)`
  is the culprit → return `(stage_index, evidence)`.
- `chain::prove_stage_fraud(stage, evidence)`: the existing transition-guest fraud
  pipeline over `evidence.window` (`step_transitions`), now also writing the stage's
  `commit.bin` bytes at `Init` (§2) → the stage fraud receipt.
- `chain::prove_chain_fraud(chain_commitment, stage_index, stage_receipt)`: run the
  `chain_fraud` guest with the stage receipt as an assumption → `chain-fraud.receipt` +
  `ChainFraudJournal`.

### 5. CLI

- `cargo raster chain audit --execution` (or a follow-on `chain challenge`): after the
  public checkpoint checks, run `detect_execution_fraud`. Clean → "no fraud"; fraud →
  print the faulty stage + window, write `fraud-evidence`.
- `cargo raster chain fraud-prove`: detect → prove stage → aggregate → write
  `chain-fraud.receipt`; print the `ChainFraudJournal`.
- `cargo raster chain fraud-verify <chain-fraud.receipt>`: verify the receipt against
  the pinned `chain_fraud` image id, check its `chain_commitment_digest` matches the
  local `ChainCommitment` digest, and check the committed inner image ids against the
  built-in known-good ids — the same three checks a settlement contract would make.

## What was removed, and why

- **The `Identity` fault variant.** The previous draft had the guest "recompute the
  public hash the checkpoint claims and assert it is inconsistent (e.g.
  `sha256(program.bin) != stage.program_commitment`)". That check is vacuous: *any*
  bytes other than the real `program.bin` fail it, so exhibiting a mismatch proves
  nothing about the checkpoint. Identity fraud is a claim that the committed identity
  differs from the *correct* program for that stage — and "correct" exists nowhere
  in-proof today; it lives in the chain spec's project references, which the
  `ChainCommitment` does not commit to. An in-proof identity fault therefore needs an
  anchored expectation first — the chain-root `Raster.lock` pinning each stage's
  `program_commitment` (future work). Until then, identity fraud remains what it already
  is: a layer-0 public check (`chain audit`, `chain.rs:307-321`). Note the *execution*
  path is not weakened by this: a prover who committed identity P but ran Q produces a
  trace that diverges from honest execution of P, which is an `Execution` fault against
  the committed identity.
- **`chain_fraud_image_id` in the journal.** Self-referential and unverifiable from
  inside; replaced by the pinning discipline above. (If a later recursion layer
  aggregates chain-fraud receipts, *that* guest commits the chain-fraud image id it
  verified — the id always lives one layer up from the guest it names.)

## Attribution & propagation — why one receipt is enough

A stage's checkpoint binds its `input_manifest_commitment`, which for a `from` input
equals the *producer's* `output_structural_commitment` (enforced by `chain audit`, and
in-proof by the `Link` variant). So a fraudulent output at stage *i* is refuted by
**stage *i*'s own execution fraud proof** — stage *i+1*, which merely consumed *i*'s
(committed) output, is not implicated. Note the guest proves "stage *i* is fraudulent",
not "stage *i* is the *earliest* fraud": any provably fraudulent stage already
invalidates the chain's result, so earliest-first is challenger policy
(`detect_execution_fraud` scans in order), not a soundness requirement. v1 attributes a
single fault; multi-fault / DAG is future work.

## Security notes

- The aggregation guest **never trusts the host's "stage i is faulty" claim** — it
  re-derives the chain digest from the bytes it reads checkpoints from and checks the
  receipt's triple against `stages[i]`, so a mis-attributed or cross-run receipt fails
  in-guest.
- `refuted_trace_commitment` closes the *"which trace?"* gap — and §2's slice check is
  what makes it sound: without it the field would merely *name* `commit.bin` while the
  divergence was proven against a host-chosen fingerprint, letting a challenger frame an
  honest stage with a receipt that names the honest commitment but refutes a fabricated
  one. With the check, `Finished` is reachable only by diverging from the named
  commitment's own fingerprint at the frontier-fixed offset. The residual soundness
  margin is the scheme's existing one — matching the committed fingerprint bits across
  the pre-divergence window (`FRAUD_DETECTION_SECURITY_BITS` = 128 revealed bits per
  window) under deterministic execution — unchanged by this proposal.
- **Image ids are threaded, never assumed** — see "image-id pinning" in §3. Every
  `env::verify` id in the design is either pinned by the relying party or committed to
  the journal that party checks.
- **The `ChainCommitment` does not commit to the chain spec.** A dishonest prover can
  record a spec-chained parameter as `External` (feeding arbitrary bytes), and no
  in-proof `Link` fault will exist — the commitment is internally consistent, just not
  the pipeline the spec describes. This is a layer-0 concern by construction: the
  relying party already holds the `ChainCommitment` (to check the digest), so checking
  its `input_bindings` against the spec it cares about is a public equality, exactly
  like checking stage count and names. A `chain_spec_commitment` folded into
  `ChainCommitment` would make it one hash check (future work).
- Checkpoint faults stay cheaper to check publicly; the `Link` variant exists only so
  one on-chain object can express either chain fault uniformly.

## Future work

- Multi-fault and DAG chains (`program-chain.md` v1 is linear).
- The dual: folding the honest per-stage receipts into one succinct *validity* proof of
  the whole chain.
- A chain-root `Raster.lock` (see the chain-workspace work) pinning each stage's
  `program_commitment` — the anchored expectation that makes an in-proof `Identity`
  fault meaningful, with no rebuild.
- A `chain_spec_commitment` in `ChainCommitment`, closing the spec-binding note above.

## Resolved questions (previous draft)

- *Widen `TransitionJournal`, or a distinct fraud-only journal?* — Widen, but bind the
  field at `Init` rather than attach it at `Finished`: the window-slice check has to
  happen when the window opens anyway, and a commitment name is meaningful on honest
  windows too, so the field is unconditional and no fraud-only type is needed.
- *Should `chain_fraud` accept the already-reduced final journal, or re-verify the full
  stage recursion?* — The reduced journal: `env::verify` already validates the whole
  recursion behind it, and the transition guest holds `transition_image_id` constant
  across that recursion, so re-walking it in the aggregation guest would add cost and
  no soundness.
