# Proposal: `chain-io-commitment` — an I/O-only chain commitment, with the trace as a dispute artifact

Status: **partly implemented** (2026-08-27) — §1's checkpoint narrowing and the journal work
it rests on have landed; §3's dispute protocol has not. See §Implementation status.

Companion to:
- [`chain-fraud-proof.md`](./chain-fraud-proof.md) (implemented) — supplies the window/commitment
  binding (§2's slice check) this design reuses unchanged, and the image-id pinning discipline.
  **Labelled disagreement** with its `StageCheckpoint.trace_commitment_digest` field; nothing else.
- [`program-chain.md`](./program-chain.md) (partly implemented) — the checkpoint this narrows.
- [`chain-stage-execution.md`](./chain-stage-execution.md) (partly implemented) — supplies the
  determinism fact the design rests on, and the `--stage` machinery the dispute path needs. Its
  §5 refusal to touch the authenticated path is lifted here, for the reason that refusal named.
- [`unauthenticated-execution.md`](./unauthenticated-execution.md) (implemented) — §10's open
  policy question. This proposal **dissolves** it rather than answering it (§9).

Depends on:
- [`window-seed-reconstruction.md`](./window-seed-reconstruction.md) (proposed) — **hard
  dependency for recur programs.** Without it a terminal window opening mid-loop is rejected, and
  the challenge in §4 is inexpressible. See attack **S3**; this is the one vector the design does
  not close on its own.

## Problem

A chain commitment costs a fully authenticated run of every stage, and the reason is one field.

`StageCheckpoint` (`crates/raster-core/src/chain.rs:33-52`) has seven members. Six are pure
functions of public artifacts, computed identically in both authentication postures:

| field | source | needs a trace? |
|---|---|---|
| `name` | the spec | no |
| `program_commitment` | `read_program_identity` — `sha256(domain ‖ program.bin)` | no |
| `input_manifest_commitment` | `synthesize_inputs` | no |
| `input_bindings` | the spec | no |
| `output_payload_commitment` | `collect_output` — `sha256(output.bin)` | no |
| `output_structural_commitment` | `collect_output` — structural root | no |
| **`trace_commitment_digest`** | `TraceCommitment::try_build(&trace, …).header().digest()` | **yes** |

`collect_output` sits under `if produces_output` (`crates/raster-cli/src/chain.rs:333`), not under
an authentication branch — `unauthenticated-execution.md` §6.1 made that deliberate and
load-bearing. So the entire I/O side of a checkpoint is already free.

The seventh field is not optional, and the consequence cascades: `trace_commitment_digest` is
`None` unauthenticated (`chain.rs:312-331`), so no `StageCheckpoint` is pushed (`chain.rs:352-369`),
so no `ChainCommitment` is written at all (`chain.rs:371-380`). **One required field makes the
chain commitment all-or-nothing authenticated.**

What that costs:

- **Authenticated posture on every stage** — 1.67 ms → 11.07 ms on `hello-tiles`, **6.6×**
  (`unauthenticated-execution.md:8`).
- **`TraceCommitment::try_build`** — one SHA-256 per trace item plus an incremental Merkle append
  per item (`crates/raster-prover/src/trace.rs:253-284`), i.e. O(#tile calls), and a trace-sized
  `commit.bin` per stage on disk.
- **Paid twice.** `detect_execution_fraud` (`chain.rs:681-742`) re-runs *every* stage
  authenticated to challenge. The commitment cost is borne once by the claimer across all stages,
  and again by every challenger across all stages.

And the field buys nothing at commit time. `output_payload_commitment` already states *what* the
stage computed, and `chain audit` already checks it against the bytes (`chain.rs:543-565`). The
trace commitment is a **bisection commitment** — it exists so that a *dispute* costs one O(window)
zkVM proof instead of a whole-stage one. It is dispute machinery, produced unconditionally for
stages nobody will ever dispute.

> The checkpoint commits to **how** the stage ran, at run time, when only **what** it computed is
> needed at run time.

## Goal

Make the chain commitment an I/O-only object, and move the trace commitment to the one place it
does work: inside a dispute, for the one stage under dispute.

1. `StageCheckpoint` loses `trace_commitment_digest`. A chain commitment becomes producible by a
   cheap run.
2. A dispute over stage *i* is opened by a **challenge that carries its own proof**, and closed
   either by the claimer's refutation or by the claimer's silence.
3. Every guest, prover, and journal mechanism the dispute uses already exists, except one field
   and one assertion.

Non-goals, stated up front:

- **Not a validity proof.** This stays optimistic. Nothing here proves a chain correct; it makes
  fraud provable and makes the honest path cheap.
- **Not a settlement layer.** `docs/specs/core/4-verify/05-on-chain-interface-assumptions.md`
  records that raster has no on-chain client, no contract bindings, and no calldata format. The
  settlement contract, artifact DA, and bonding are **planned and assumed** — see §Assumed
  infrastructure. This proposal specifies the objects and the decision rule that sit on top of
  them, and marks every attack that reduces to one of them **[infra]**.
- **Not DAG chains, not multi-fault.** `program-chain.md` v1 is linear and stays linear; a
  challenge names one stage.
- **`ChainFaultKind::Link` is untouched.** It is a checkpoint-internal inconsistency with no trace
  involved, fully in-proof and self-contained today. It stays exactly as it is.

## Facts the design builds on (verified in code)

| # | Fact | Where |
|---|---|---|
| F1 | Six of seven `StageCheckpoint` fields are posture-independent; `collect_output` is not behind an auth branch | `raster-cli/src/chain.rs:333`; `unauthenticated-execution.md` §6.1 |
| F2 | The all-or-nothing interlock is `trace_commitment_digest` being non-`Option` on the checkpoint | `chain.rs:312-331`, `:352-380` |
| F3 | **A stage's execution is a pure function of `(program, input.json, input_manifest.json)`** — and re-running one stage alone reproduces a **byte-identical `output.bin`** | `chain-stage-execution.md` §Facts + §Verification (`tests/chain_stage_cli.rs`) |
| F4 | `verify_program_end` already verifies the terminal output commitment against a storage-read + selection witness — and **discards the value** | `guests/transition/src/checks/entrypoint.rs:175-245`, assert at `:239` |
| F5 | `OutputAuthorization` is a bare three-state discriminant — `NotRequired`, `Pending`, `Established` — carrying no value | `raster-core/src/transition.rs:298-306` |
| F6 | The **input** end of the journal *is* exported by value — `input_manifest_commitment` — and `verify_step` checks the `ProgramStart` binding against it | `transition.rs:315`; `entrypoint.rs:73-108` |
| F7 | **A terminal window stands alone.** `verify_genesis_authorization`'s membership-witness route establishes `EntrypointAuthorization::Established` against the window's *initial* storage state — no recursion back to genesis | `entrypoint.rs:126-152` |
| F8 | `refuted_trace_commitment` + the §2 slice check already bind a window's fingerprint to a named commitment at a frontier-derived offset | `transition.rs:323-331`; `chain-fraud-proof.md` §2 |
| F9 | `ProgramEnd` is terminal *within a window* (`next_expected_coordinates = Vec::new()`), but **the window's position within the commitment is unpinned** | `guests/transition/src/fraud_proof.rs:513-527` |
| F10 | `TraceVerifier::verify` returns `VerificationResult::Fraud(FraudEvidence)` carrying the offending window | `raster-prover/src/trace.rs:852`, `:916`, `:802-810` |
| F11 | The challenger already re-runs every stage authenticated today | `chain.rs:681-742` |
| F12 | `bits_per_item = ceil(128 / window_size)`, floored at 1; the commitment header self-declares its `bits_packer` and `fingerprint_len` | `trace.rs:157-184`; `transition.rs:66-73` |
| F13 | The trace-tree seed is a hardcoded constant on both sides | `trace.rs:77`; `guests/transition/src/merkle_tree.rs:24` |
| F14 | `--stage` / `--run` are clap-gated to `--no-auth` | `raster-cli/src/main.rs:237`, `:243` |
| F15 | A fraud-proof window opening inside a live recur loop is **rejected** today (completeness gap, not a soundness one) | `window-seed-reconstruction.md` §Problem |

F4–F6 together are the whole design. The journal binds the input end of the execution by *value*
and the output end by *status*. The bridge between them is anchored at one end.

## Threat model

Three parties, none trusted except where stated.

- **A dishonest claimer** publishes a `ChainCommitment` whose stages misexecuted, mislinked, or
  misdeclared. The protocol must be **complete**: every such fault must be answerable by an honest
  challenger holding only the public artifacts.
- **A dishonest challenger** tries to condemn an honest chain, or to make honest participation
  expensive. The protocol must be **sound**: no honest chain may be condemned; and it must be
  **non-griefable**: a false accusation must cost the accuser at least as much as the defence.
- **The relying party** holds the `ChainCommitment`, pins guest image ids out of band, reads a
  clock, and performs public equality checks. It never trusts a host claim.

The design assumes, unchanged from today: deterministic execution; public availability of
`program.bin`, `input.json`, `input_manifest.json`, and each stage's `output.bin`; and the
existing fingerprint soundness margin of `FRAUD_DETECTION_SECURITY_BITS = 128` bits per window
(`trace.rs:129`).

### Assumed infrastructure

Three things this proposal specifies against but does not build. They are **planned**, and the
design is written on the assumption they arrive together — which they must, since the dispute
protocol is inert without the first two.

1. **A settlement contract with a clock.** Condemnation-by-silence needs a response window `T`
   and something that observes its expiry. Every variant of this protocol needs it; there is no
   version that runs without one.
2. **Data availability for artifacts.** The challenged commitment's body must be retrievable by
   the claimer. The `TraceCommitmentHeader` already content-addresses it (`fingerprint_root`,
   `revealed_items_commitment`), so the binding is free — what is needed is a layer that holds
   the bytes and an admission rule that will not start `T` until they are retrievable.
3. **Bonding on both sides**, to price griefing that cryptography does not prevent.

Attacks that reduce to one of these are marked **[infra]** in §Attack surface: recorded, not
closed here, and not treated as blockers. `docs/specs/core/4-verify/05-on-chain-interface-assumptions.md`
records that none of the three exists in the toolchain today.

**What the clock-and-bond arriving together settles.** An earlier reading of this design treated
"bonds do not exist yet" as a reason to prefer the accuser-pays rule below. That reasoning is
void: the clock and the bond are the same piece of infrastructure, so a world with a timeout but
no bond was never going to exist. The rule in §3 is chosen on its merits instead — see
§Alternatives, "cheap challenge, claimer disproves", which is where the argument actually lands.

## Design

Four changes. Three are small; the fourth is a protocol.

### 1. `raster-core` — the checkpoint narrows, the journal symmetrizes

**`StageCheckpoint` drops `trace_commitment_digest`.** Nothing replaces it. No fraud-proof
parameter block is added: the challenger's commitment self-declares its `bits_packer` and
`fingerprint_len` in the header the guest already checks against (F12), and the seed is a
constant (F13), so there is no canonical trace commitment for the checkpoint to name.

**`OutputAuthorization::Established` gains the value it already proves** (F4, F5):

```rust
pub enum OutputAuthorization {
    NotRequired,
    Pending,
    /// A `ProgramEnd` step has been verified in this chain: the committed
    /// output provably lives in committed storage, and this is the value it
    /// committed. Symmetric with `input_manifest_commitment`, which names the
    /// authorized *input* the same window is bound to.
    Established { output_commitment: Vec<u8> },
}
```

Threaded through `commit_journal` and inherited across `Next` exactly as `refuted_trace_commitment`
is (`chain-fraud-proof.md` §2 item 4 is the precedent). The `Next` continuity assertion must
compare **payloads, not discriminants** — otherwise a `Next` step could relabel the output.

**Two new shared types** (in `raster-core`, so the host, the CLI, and the aggregation guest agree):

```rust
/// A challenge against one stage of a published chain.
///
/// The receipt *is* the challenge. Every value a verifier compares is read out
/// of `terminal_journal`, which a guest produced — there is no field here the
/// challenger merely asserts. See §Alternatives, "the counter-checkpoint".
pub struct StageChallenge {
    /// The exact `chain-commitment` bytes; the digest is recomputed from these
    /// before checkpoints are read out of them.
    pub chain_commitment_bytes: Vec<u8>,
    pub challenged_stage: u32,
    /// Terminal-window transition receipt over the challenger's commitment.
    /// Supplies H_C (`refuted_trace_commitment`), O'
    /// (`output_authorization`), and the program/manifest identity.
    pub terminal_journal: TransitionJournal,
    pub transition_image_id: Vec<u8>,
    /// Where the challenged commitment's body may be fetched. The header
    /// digest content-addresses it; see attack H7.
    pub commitment_availability: CommitmentAvailability,
}

/// "Challenge C against chain D stage i is itself fraudulent." Produced when
/// the claimer refutes. Condemns the challenge, not the chain.
pub struct ChallengeFraudJournal {
    pub chain_commitment_digest: [u8; 32],
    pub challenged_stage: u32,
    /// == the challenge's `terminal_journal.refuted_trace_commitment`.
    pub challenged_trace_commitment: Vec<u8>,
    pub transition_image_id: Vec<u8>,
}
```

`ChainFraudJournal` and `ChainFaultKind` stay, narrowed to `Link` (§5).

### 2. Transition guest — report terminality

`Established { X }` must not name the output of *a* `ProgramEnd` sitting somewhere inside the
commitment — it must name *the* trace's result. That needs one more fact: the window ends where
the commitment ends.

**The check is a derivation at `Init`, not an assertion at `ProgramEnd`.** Implementation found
the obvious site unavailable: `FraudProofWindowContext::proceed` consumes the
`TraceCommitmentHeader` in its `Init` arm and returns only `header.digest()`, and
`apply_verified_step` — where the `ProgramEnd` branch lives — never receives the window context.
The header is simply not in scope there.

At `Init` it is, and so is everything else. `assert_window_is_commitment_slice` already holds
`window_start` (frontier-derived, never host-claimed), `window_len`, and `header.fingerprint_len`
together — it already asserts `window_start + window_len <= fingerprint_len`. So it returns the
equality case:

```rust
// `<=` was asserted above; equality is the terminal case.
window_start + window_len == fingerprint_len
```

carried on `FraudProofWindowContext` beside `refuted_trace_commitment`, committed to the journal
as `window_is_terminal`, and inherited across `Next` the same way.

**The two facts are combined by the consumer, not the guest.** `ProgramEnd` already forces
`next_expected_coordinates` empty (F9), so nothing may follow it *within* a window — a window
containing one ends with it. Add `window_is_terminal` and that `ProgramEnd` is the commitment's
last item too. A consumer requiring `Established { X } && window_is_terminal` therefore gets the
trace's actual result, with **no new guest assertion and no plumbing into `apply_verified_step`**.

Nothing else in the guest changes. The slice check's own assertions, the divergence logic,
`finalize`, `assert_program_continuity`, and `assert_manifest_continuity` are untouched.

### 3. The dispute protocol

**Opening.** A challenger who believes stage *i*'s output is wrong:

1. Re-runs the chain **unauthenticated** and finds the first stage where
   `sha256(output.bin) ≠ output_payload_commitment`. Cheap (F3, F1).
2. Re-runs stage *i* **authenticated, in place** (`chain run --stage i`, §4), producing `T_C` and
   its header digest `H_C`.
3. Proves the **terminal window** of `T_C` — one O(window) transition-guest run, standing alone
   via the membership-witness route (F7).
4. Publishes the `StageChallenge` and makes `T_C`'s body available.

**Admission** — public equality checks, no proving. With `j = terminal_journal`,
`s = stages[challenged_stage]`, and the chain digest recomputed in-guest from
`chain_commitment_bytes` before the checkpoints are read out of them:

```
verify(transition_image_id, j)                                   -- the receipt is real
j.transition_image_id        == transition_image_id              -- and self-consistent
j.program_commitment         == s.program_commitment             -- same program
j.input_manifest_commitment  == s.input_manifest_commitment      -- same authorized inputs
j.entrypoint_authorization   ∈ { Established, NotRequired }      -- the input end is bound
j.output_authorization       == Established { O' }               -- the output end is bound
O'                           != s.output_payload_commitment      -- and they disagree
header(H_C).bits_packer      >= BITS_PER_ITEM_FLOOR              -- attack H5
```

Read top to bottom, that is **input commitment → trace → output commitment**, with every arrow
checked in-guest rather than asserted in a document.

**Closing.** Within the response window `T`, the claimer must publish a `Finished` transition
journal with `refuted_trace_commitment == H_C` and matching program/manifest — the existing fraud
pipeline (F10), retargeted from the checkpoint's `commit.bin` to the challenger's commitment.

| claimer's response | outcome | evidence object |
|---|---|---|
| a valid refutation | challenge dismissed | `ChallengeFraudJournal` — succinct, self-contained |
| nothing, before `T` expires | **chain condemned, stage *i* to blame** | `(StageChallenge, expiry)` — succinct, but needs a clock |

The asymmetry in that second row is the design's real price and §7 states it plainly.

**Why the challenger carries the proof.** The accuser is already paying the dominant cost — an
authenticated re-run of the stage, plus publishing its commitment body for DA. One O(window)
terminal proof on top of that is marginal, and it buys three structural properties a bond can
only buy economically: mislabelling becomes impossible rather than punishable (**H1**), a
fabricated commitment cannot be admitted at all rather than being refuted at the claimer's
expense (**H12**), and an *offline* honest claimer is condemned only by a substantiated
accusation. See §Alternatives for the full comparison against the cheap-challenge rule, which is
cheaper by exactly one window proof and gives up all three.

### 4. `raster-prover` host + CLI

- **Lift the `--no-auth` gate** on `--stage` / `--run` (F14), with the guard the gate was standing
  in for: an authenticated single-stage run writes `commit.bin` and **must not** write a one-stage
  `ChainCommitment` over the existing one.
- `chain::detect_output_fraud(chain_dir, spec)` — the unauthenticated scan of step 1. Replaces
  `detect_execution_fraud`'s all-stages-authenticated loop (F11).
- `chain::prove_terminal_window(stage_dir)` — the challenge receipt of step 3.
- `chain::refute(challenge)` — `TraceVerifier` against the challenged commitment, then the existing
  `step_transitions` fraud pipeline.
- CLI: `chain challenge <stage>`, `chain respond <challenge>`, `chain challenge-verify <challenge>`.
  `chain audit` keeps its layer-0 checks minus check 3 (`chain.rs:568-585`, the `commit.bin`↔digest
  binding, which has no artifact to bind); `chain audit --execution` becomes an output-hash
  comparison over unauthenticated re-runs.

### 5. `chain_fraud` guest

`ChainFaultKind::Execution` is **removed**: execution fraud is no longer provable as a
self-contained receipt (§7). `Link` is unchanged — it needs no trace and was always in-proof.

A sibling aggregation produces `ChallengeFraudJournal` from a refutation: verify the `Finished`
journal, assert `refuted_trace_commitment == challenge.terminal_journal.refuted_trace_commitment`
(the challenge's own H_C, a public input) plus the program/manifest equalities against
`stages[i]`. This is the existing `Execution` variant with one comparand changed.

Image-id pinning is unchanged from `chain-fraud-proof.md` §3: the relying party pins the outer
image id out of band; inner ids are committed to the journal and checked.

## Attack surface

Every vector, and what closes it.

### A dishonest claimer

| # | Attack | Closed by |
|---|---|---|
| **C1** | Commit output `O` when honest execution yields `O'` | Challenger's terminal receipt proves `O' ≠ O` over the committed program+inputs; the claimer can neither refute an honest `T_C` nor stay silent without being condemned. **This is the base case.** |
| **C2** | Commit identity `P` but run `Q` | Honest execution of `P` on the committed inputs either differs from the committed output → C1, or agrees → no harm, since the output is what `P` produces. Unchanged from `chain-fraud-proof.md` §What was removed. Layer-0 `chain audit` still catches `P` ≠ the declared source (`chain.rs:527-541`) |
| **C3** | Feed a `from` parameter something other than the producer's output root | `ChainFaultKind::Link`, untouched — `detect_link_fraud` (`chain.rs:752`) plus the authorization-journal path |
| **C4** | Fabricate a refutation of an honest `T_C` | Requires step records that chain from `init_frontier`, pass every CFS/storage/tile-replay check, **and** produce a fingerprint differing from `H_C`'s at the frontier-fixed offset. The §2 slice check binds the window to `H_C.fingerprint_root` (F8). For a deterministic program, valid records reproduce the honest fingerprint. Residual: the existing 128-bit/window margin |
| **C5** | Answer with a receipt about a different stage or a different run | Journal's `program_commitment` and `input_manifest_commitment` must equal `stages[i]`'s — the equality pattern `chain_fraud` already uses |
| **C6** | Withhold `commit.bin` to block an audit | **Vector removed.** There is no `commit.bin` in a checkpoint to withhold; today a claimer can publish a `ChainCommitment` and never release the artifacts check 3 needs |
| **C7** | Refute using a window that opens before the divergence to dodge it | `finalize` reaches `Finished` exactly at a divergence at the *final* window index (`fraud_proof.rs:421-446`); a window with no divergence produces no `Finished` journal. Unchanged |

### A dishonest challenger

| # | Attack | Closed by |
|---|---|---|
| **H1** | **Mislabel.** Publish the claimer's *honest* trace (computable — inputs and program are public) and assert a garbage `O'` | The challenge is a **receipt, not a document**: `O'` is read out of `output_authorization`, which the guest derived from the `ProgramEnd` step bound to `H_C`. There is no field to mislabel. *This is the vector that kills the counter-checkpoint alternative* (§Alternatives) |
| **H2** | Fabricate a trace ending in `O'` | Claimer refutes: it diverges from honest execution, and `TraceVerifier` locates the window (F10) |
| **H3** | Run stage *i* on inputs other than the committed ones | `j.input_manifest_commitment ≠ s.input_manifest_commitment` — the challenge fails admission before any response |
| **H4** | Run a different program | `j.program_commitment ≠ s.program_commitment` — same |
| **H5** | Choose `bits_per_item = 1` to inflate the refuter's minimum window to 128 items | Not a soundness break (the divergence stays findable, and `MAX_FRAUD_PROOF_WINDOW_SIZE = 1024` leaves headroom) but a cost grief. Closed by `BITS_PER_ITEM_FLOOR` at admission (F12) |
| **H6** | Truncate or extend `fingerprint_len` so the "terminal" window lands mid-execution or on padding | The terminality pin (§2) plus the requirement that admission see `Established`: a commitment whose last item is not a `ProgramEnd` yields `Pending`, so no valid challenge exists. A commitment truncated *at* a fabricated mid-trace `ProgramEnd` diverges from honest execution at that index → refutable |
| **H7** | **[infra]** Withhold the fingerprint body. Publish `H_C` and the receipt, release nothing. The claimer cannot run `TraceVerifier` (which takes the whole `TraceCommitment`, `trace.rs:813`) and is condemned by timeout while honest | The planned DA layer, plus an admission rule that will not start `T` until the body is retrievable. The header already content-addresses it, so nothing here needs designing — only the layer. §Assumed infrastructure 2 |
| **H8** | **[infra]** Grief: many challenges, forcing many defences | Bonds (§Assumed infrastructure 3). Cryptographically self-limiting *as well*, which is a property worth keeping rather than spending: each accusation costs an authenticated stage re-run, a DA publication, and a window proof — strictly more than the defence it forces |
| **H9** | Attribute a valid receipt to the wrong stage index | Equality checks resolve `stages[i]` from the challenge's own `challenged_stage`; a receipt over stage 3's program+manifest cannot match stage 5's |
| **H10** | Replay an old challenge against a newer chain | The digest is recomputed in-guest from `chain_commitment_bytes`, and checkpoints are read from those same bytes — the `chain_fraud` pattern, unchanged |
| **H11** | Use a self-compiled guest so `verify` proves nothing | Image-id pinning, unchanged (`chain-fraud-proof.md` §3): the relying party pins ids out of band and checks the committed inner ids |
| **H12** | Publish a tiny fabricated "trace" (5 items, ending in a `ProgramEnd`) with a fabricated initial storage state | Admission requires the entry binding to match the authorized manifest (F6/F7), and the storage roots live inside the step records, hence inside the fingerprint the slice check pins. The claimer refutes at the first differing index. Cost-wise it is an H8 grief, priced the same way |

### Structural and liveness

| # | Vector | Status |
|---|---|---|
| **S1** | The `ChainCommitment` does not commit to the chain spec — a claimer can record a spec-chained parameter as `External` and no `Link` fault exists | **Carried forward unchanged** from `chain-fraud-proof.md` §Security notes. Layer-0 by construction: the relying party holds the commitment and checks `input_bindings` against the spec it cares about. A `chain_spec_commitment` remains future work |
| **S2** | A **terminal unit-output stage** commits no output (`chain.rs:41-46`, `OutputAuthorization::NotRequired`), so it has no I/O disagreement to challenge | **Accepted, and correct.** Non-terminal stages must produce output (`chain.rs:262-268`), so only the terminal stage can be unit — and a unit terminal stage contributes nothing to the chain's result. Stated explicitly rather than left implicit, because today such a stage *is* covered by `chain audit --execution` |
| **S3** | **A claimer picks a program whose terminal window opens mid-recur, becoming unchallengeable** — such a window is rejected today (F15) | **Not closed by this proposal.** Hard dependency on `window-seed-reconstruction.md`. For a recur-heavy stage the terminal window will very often open inside a live loop, so this is the common case, not the exotic one. **Ship together** |
| **S4** | **[infra]** Condemnation by silence needs a clock, and a liveness assumption about the claimer | The settlement contract (§Assumed infrastructure 1). Inherent to removing the trace from the checkpoint (§7) — every variant of this protocol has it. The accuser-pays rule bounds the *harm*: an offline claimer is condemned only when the accusation carried a real terminal receipt |
| **S5** | Both parties may pick their own window within the commitment | Already true today and bounded by the same mechanisms: the slice check pins the window's offset and contents to `H_C`, and terminality is pinned by §2 |

## What this breaks

- **`ChainCommitment` digests move.** `StageCheckpoint` loses a field, so every recorded chain
  digest changes. There is no migration; old commitments are re-derived by re-running, which is
  now cheap.
- **`chain audit` loses check 3** (`chain.rs:568-585`) — there is no `commit.bin` to bind.
- **`chain audit --execution` changes meaning** from "every stage's trace matches its commitment"
  to "every stage's output matches its commitment". Strictly cheaper and equally decisive at the
  chain level; it stops localizing *where inside a stage* a divergence occurred without the
  claimer's participation.
- **`ChainFaultKind::Execution` is removed.** See §7.
- **Guest image ids move** — `raster-core` is linked into the guests, and `OutputAuthorization`
  changes shape. Same class of break as `incremental-draft-witness`.
- **`chain-repeat.md` collides.** It adds `ChainShape` to `ChainCommitment`; this removes a
  checkpoint field. Both move the digest, so whichever lands second inherits the other's change.

## Security notes

- **The core argument is two-part, and neither part alone suffices.** The challenge's receipt
  proves *"`H_C` is a trace over stage i's committed program and authorized inputs, and it
  terminates in `O' ≠ O`."* It does **not** prove `H_C` is honest. The claimer's silence
  establishes *"nobody can exhibit a divergence in `H_C`."* Together: honest execution terminates
  in `O' ≠ O`, so the checkpoint is fraudulent. The second half is optimistic, which is the
  system's existing posture, not a new assumption.
- **Publishing a value is not binding it.** The design's central discipline, and the reason
  §Alternatives rejects the counter-checkpoint. `chain-fraud-proof.md` §Problem 3 learned this one
  level down: `refuted_trace_commitment` already *named* a commitment, and naming was not enough —
  it took §2's in-guest slice check to make the name mean something. Adding `O'` beside `H_C` in a
  challenger-authored struct recreates exactly that pattern.
- **The soundness margin is unchanged**: 128 fingerprint bits per window under deterministic
  execution (`trace.rs:129`). Nothing here widens or narrows it.
- **The standalone terminal window relies on the membership-witness route** (F7), whose initial
  storage root is pinned to `H_C` only through the step records inside the fingerprint — i.e.
  within the same 128-bit margin. This is the existing assumption, applied at a new site.
- **Image ids are threaded, never assumed** — unchanged from `chain-fraud-proof.md` §3.

## Alternatives considered

- **Keep `trace_commitment_digest`, make it per-stage optional (mixed posture).** This is
  `unauthenticated-execution.md` §10's framing. Rejected: it forces the question §10 flags — what
  a chain commitment means when its stages ran at different postures — makes `StageCheckpoint` a
  sum type, makes the digest depend on a policy choice, and needs a rule for the case where the
  stage you must commit is one nobody committed. §3 makes the posture uniform and never asks it.
- **Derive a canonical trace commitment from the checkpoint** (an earlier form of this proposal).
  Because execution is deterministic over committed inputs (F3), the honest trace commitment of
  stage *i* is uniquely determined — so it could be reconstructed rather than recorded, and the
  *defender* compelled to publish it when challenged. Rejected in favour of §3: it costs an extra
  round (challenge → compelled publication → proof, versus challenge-with-proof → proof), and it
  requires pinning `FraudProofParams` in the `ChainCommitment` so "canonical" is well-defined. The
  challenger-publishes form needs neither, because the header self-declares its parameters (F12).
- **The counter-checkpoint (`StageClaim`).** Have the challenger publish a full checkpoint-shaped
  object — same fields as `StageCheckpoint`, extended with `H_C` — so the challenge "commits to
  the whole result". **Rejected: it does not bind anything.** A dishonest challenger writes `H_C` =
  the claimer's honest trace and `O'` = garbage; the document is complete, well-formed, and hashes
  fine, but the adjacency of the two values is a lie the claimer cannot refute (attack **H1**).
  Adjacency in a struct one party authored is two independent claims, not a relation. The shape is
  right and is kept — as the *journal* of the terminal receipt, where every field is guest-derived.
- **Put `output_commitment` inside `TraceCommitmentHeader`** so `H_C` covers it. Same failure: the
  header is host-built (`TraceCommitmentExt::header`), so this moves the unbound field inside the
  digest without checking it. The guest's slice check verifies the *fingerprint* against
  `fingerprint_root` and never reads the new field. Generally: *"this trace produces that output"
  is a statement about the trace's contents, and the only thing that reads trace contents is the
  guest* — hashing more fields together produces a bigger unverified claim.
- **Cheap challenge, claimer disproves.** Challenger publishes a document asserting `H_C` and
  `O'`; the claimer answers with *either* a refutation *or* a **mislabel proof** (`H_C`'s terminal
  output is `X ≠ O'`). Sound and complete — verified by case analysis — and it catches real fraud
  for **zero** zkVM proofs, against one for the rule in §3. Rejected on the margin, and the
  reasoning is worth recording because the obvious one is wrong.

  The obvious objection is griefing: free accusations, expensive defences. That objection is
  **void once bonding is planned** (§Assumed infrastructure), and it was the stated reason an
  earlier draft deferred this rule. The real argument is a cost comparison:

  An honest challenger must, under *either* rule, re-run the stage authenticated and publish its
  commitment body for DA. Both dominate one O(window) terminal proof (window ≤ 1024 items,
  `MAX_FRAUD_PROOF_WINDOW_SIZE`). So the saving is **one marginal proof on top of a cost already
  paid**, and what it spends is three structural properties:

  - **H1 returns as a live vector.** `O'` becomes an unproven assertion again, defended by move
    (b) rather than made impossible. Sound, but it is the pattern `chain-fraud-proof.md`
    §Problem 3 exists to eliminate, reintroduced deliberately.
  - **H12 becomes a bond problem.** A fabricated commitment — cheap, since a fingerprint of
    random bits requires running nothing — is admissible and must be refuted at the claimer's
    expense. Under §3 it cannot produce a valid terminal receipt and never reaches admission.
  - **An offline honest claimer is condemned by a document that proves nothing.** Bonds punish a
    false challenge only if someone collects, and an offline claimer cannot. Under §3 the same
    claimer is condemned only by an accusation that at least proves *"a trace over this program
    and these authorized inputs terminates in `O' ≠ O`"*.

  Converting cryptographic guarantees into economic ones to save a marginal proof is the wrong
  direction. Recorded rather than dismissed: if measurement shows the terminal proof is *not*
  marginal against the stage re-run for real workloads, the trade flips, and it flips cheaply —
  both rules need the identical `Established { output_commitment }` change, and only the public
  comparison differs (`X ≠ O'` instead of `O' ≠ s.output_payload_commitment`). No guest change.
- **Intra-stage segment anchors.** Commit *K* intermediate I/O commitments per stage so a dispute
  bisects stage → segment → window, for stages too large to re-run at dispute time. Genuinely
  useful and orthogonal; deferred to §Future work because it only pays once a single stage's
  re-run is measurably the bottleneck, and because `recur-progress-commitment.md`'s
  *commit-and-recompute requires input parity* rule makes anchor placement its own design problem.

## Implementation plan

Seven phases in three groups. **A** (1–3) changes what a receipt *says* and is landable now, with
the current protocol still working end to end. **B** (4–5) narrows the checkpoint — the point of
no return. **C** (6–7) is the dispute protocol, which cannot be exercised until the settlement
layer exists (§Assumed infrastructure) but can be built and tested against a simulated clock.

Phase 1 must land first; 2 depends on it; 3 is independent of 2. Nothing in **B** or **C** may
start before **A** is green.

### Group A — the receipt says more (no protocol change)

#### Phase 1 — the journal carries the output value

`OutputAuthorization::Established { output_commitment: Vec<u8> }` (F4, F5).

| # | Change | Site |
|---|---|---|
| 1.1 | Add the payload to the variant; update the doc comment | `raster-core/src/transition.rs:298-306` |
| 1.2 | **Drop `Copy` from the derive** and repair the fallout | `transition.rs:297`; `guests/transition/src/fraud_proof.rs:351, 362, 382, 396, 416, 436-437, 611, 625` |
| 1.3 | Return the value `verify_program_end` already asserted equal to `output.selection.selected_hash` | `guests/transition/src/checks/entrypoint.rs:239-245` |
| 1.4 | `Next` continuity compares **payloads, not discriminants** | `fraud_proof.rs:188` |
| 1.5 | `Init` still sets `Pending` / `NotRequired` — unchanged | `fraud_proof.rs:158-166` |

**Named risk — 1.2 is the whole cost of this phase.** `OutputAuthorization` is `Copy` today and
`fraud_proof.rs:436-437` returns it by value; a `Vec<u8>` payload breaks that at eight sites in
one file. All mechanical (`.clone()` or borrow), all in one place, but do them as their own commit
so the semantic change in 1.3–1.4 reviews cleanly. A `Box<[u8; 32]>`-style fixed payload would
keep `Copy` — rejected, because every other commitment in these types is `Vec<u8>` and one
exception is worse than one clone.

**Exit criteria.** Existing guest and prover suites green. New tests: a window covering
`ProgramEnd` yields `Established { O }` with `O` equal to the step's `output_commitment`; a `Next`
step presenting a different payload is rejected. Guest image ids move — expected, same class as
`incremental-draft-witness`.

#### Phase 2 — the terminality pin

`Established` must imply *"this window ends where the commitment ends"* (§2, F9), or `O'` names
the output of *a* `ProgramEnd` inside the commitment rather than *the* trace's result.

The obstacle is plumbing: the header is read at `Init`, but the pin must fire at the `ProgramEnd`
step (`fraud_proof.rs:485`), and `s = init_frontier.position` / `w = init_state.fingerprint.len()`
are available there while the commitment's length is not.

**Decision — carry it on `InitTransition`.** Add `commitment_fingerprint_len: u64`
(`transition.rs:238-245`), set at `Init` from the same header the slice check already reads, and
assert `header.fingerprint_len == init_state.commitment_fingerprint_len` there so it is a checked
fact rather than a host claim. `init_state` is a journal field distinct from `current_state`, so
it survives `Next` and is in scope at 485:

```rust
// at the ProgramEnd step, before returning Established
assert_eq!(
    init_frontier.position + init_state.fingerprint.len() as u64,
    init_state.commitment_fingerprint_len,
    "window is not terminal in this commitment",
);
```

Alternative considered and rejected: a `window_is_terminal: bool` computed at `Init`. Same size,
but a flag whose meaning lives in another function is worse to audit than the length it was
derived from.

**Exit criteria.** A window ending at `fingerprint_len` yields `Established`; one ending earlier
is rejected *even when it contains a `ProgramEnd`* — this is the test that makes the phase real.
Non-terminal windows keep today's `Pending` behaviour.

#### Phase 3 — host plumbing (parallel with 2)

- `raster-prover/src/chain.rs`: `prove_terminal_window(stage_dir) -> TransitionJournal + receipt`
  — the existing transition pipeline aimed at the final window, supplying the membership witness
  for the standalone route (F7) rather than recursing to genesis.
- Retarget `TraceVerifier::new` construction so the commitment comes from a caller-supplied
  `TraceCommitment` rather than `stage_dir/commit.bin` (`chain.rs:726-731`).
- `raster-cli`: **lift the `--no-auth` gate** on `--stage` / `--run` (F14, `main.rs:237, 243`),
  with the guard that gate stood in for — an authenticated single-stage run writes `commit.bin`
  and **must not** write a one-stage `ChainCommitment` over the existing one (`chain.rs:371-390`).

**Exit criteria.** `chain run --stage <name>` authenticated reproduces the stage's `commit.bin`
byte-for-byte against the whole-chain run, and leaves `chain-commitment` untouched. This extends
`chain-stage-execution.md`'s byte-identical-`output.bin` test to the authenticated path and is
the fact Phase 4 depends on.

### Group B — the checkpoint narrows (point of no return)

#### Phase 4 — drop `trace_commitment_digest`

- Remove the field (`raster-core/src/chain.rs:47-51`) and its producer (`chain.rs:312-331`).
- Collapse the `Option` interlock so a cheap run writes a full `ChainCommitment`
  (`chain.rs:352-380`).
- `chain audit` loses check 3 (`chain.rs:568-585`).
- `detect_execution_fraud` (`chain.rs:681-742`) → `detect_output_fraud`: unauthenticated re-run,
  compare `sha256(output.bin)` against the checkpoint. Drops `StageExecutionFraud`'s trace fields.

**Exit criteria — the claim the whole proposal rests on.** A chain run **cheaply** and a chain run
**authenticated** produce **byte-identical `ChainCommitment` bytes** on `examples/chain-example`.
Plus the cost delta against the 6.6× baseline, reported for both `hello-tiles` and the
three-stage chain.

#### Phase 5 — S3, the in-repo blocker

Land [`window-seed-reconstruction.md`](./window-seed-reconstruction.md). Until it does, a terminal
window opening inside a live recur loop is rejected (F15), so a recur-heavy stage is
**unchallengeable** — a claimer could select such a program deliberately.

Sequenced here rather than earlier because Phases 1–4 neither need it nor make it worse; it gates
the protocol in Group C, not the narrowing. It is separately owned, so treat it as a dependency to
schedule, not work to absorb.

**Exit criteria.** The S3 regression test in §Verification flips from expected-failure to passing:
a recur-heavy stage's terminal window proves.

### Group C — the dispute protocol

#### Phase 6 — protocol objects and CLI

- `raster-core/src/chain.rs`: `StageChallenge`, `CommitmentAvailability`, `ChallengeFraudJournal`;
  `BITS_PER_ITEM_FLOOR` (Open question 4 settles its exact form).
- Admission checks as a single host function over `(StageChallenge, ChainCommitment)` — the eight
  equalities in §3, no proving.
- `chain::refute(challenge)` — `TraceVerifier` against the challenged commitment, then the
  existing `step_transitions` fraud pipeline.
- CLI: `chain challenge <stage>`, `chain respond <challenge>`, `chain challenge-verify`.

**Exit criteria.** The attack table is the test list. Every rejected-at-admission vector (H3, H4,
H5, H6, H9, H10, H12) asserts *which* check fired. **H1 is the decisive one**: constructing a
challenge from the claimer's own honest `commit.bin` with a fabricated `O'` must be impossible,
because the receipt's `Established` carries the honest output and the
`O' != s.output_payload_commitment` check fails.

#### Phase 7 — `chain_fraud`

Narrow `ChainFaultKind` to `Link` (unchanged code, removed variant). Add the challenge-refutation
aggregation producing `ChallengeFraudJournal`: the existing `Execution` variant with one comparand
changed — `refuted_trace_commitment` compared against the challenge's `H_C` (a public input)
instead of `stage.trace_commitment_digest` (a field that no longer exists).

**Exit criteria.** C1 and H2 end to end on `examples/chain-example`: tamper stage 2's `output.bin`,
`chain challenge`, assert the claimer's refutation attempt fails and a simulated timeout condemns;
then publish a fabricated `T_C`, assert the claimer refutes and the `ChallengeFraudJournal`
verifies.

### Sequencing summary

```
Phase 1 ──► Phase 2 ──┐
        └─► Phase 3 ──┴──► Phase 4 ──► Phase 6 ──► Phase 7
                                   ▲
        window-seed-reconstruction ┘  (Phase 5 — separately owned)

              settlement + DA + bonds ─────────────► exercising C
                 (§Assumed infrastructure)            (not building it)
```

**What is measurable when.** The 6.6× saving lands at Phase 4 and is reportable there. Open
question 2 — whether the terminal proof is marginal against a stage re-run, which is what decides
§3's rule versus the cheap-challenge rule — becomes measurable at **Phase 3**, before any protocol
object is frozen. Take that measurement then; it is the cheapest point at which the design's one
live cost assumption can be falsified.

## Implementation status

Landed 2026-08-27 — phases 1–4 of the plan, i.e. everything except §3's dispute protocol.

| | state |
|---|---|
| `OutputAuthorization::Established { output_commitment }` (§1) | **landed.** `verify_program_end` returns the value it already checked instead of discarding it. `Copy` came off the enum — one real breakage (the `LiveTransition` accessor), not the eight predicted; the rest were moves |
| `window_is_terminal` (§2) | **landed**, as an `Init`-derived journal field rather than a `ProgramEnd`-time assertion — see §2 for why the obvious site was unavailable |
| `TraceVerifier::terminal_window` | **landed.** Shares `verify`'s walk; a host test asserts the guest's own terminality equality, and a deliberate frontier misalignment makes it fail |
| `StageCheckpoint` loses `trace_commitment_digest` (§1) | **landed.** Both postures now build checkpoints, so `chain run --no-auth` writes a real `ChainCommitment` |
| `chain audit` commitment-binding check | **removed** — no checkpoint field left to compare |
| `detect_execution_fraud` → `detect_output_fraud` | **landed.** Cheap unauthenticated re-run, output-hash comparison |
| `ChainFaultKind::Execution` | **removed**, with the replacement below |
| §3 challenge/response, `StageChallenge`, `ChallengeFraudJournal` | **not built.** Inert without a settlement clock |

**Two decisions the implementation forced, neither in the original design.**

1. **Identity in the cheap posture.** A checkpoint needs `program_commitment`, so a cheap run now
   reads program identity — which `unauthenticated-execution.md` §10 deliberately skipped, because
   that mode exists for the case where identity *cannot* be resolved (a source change whose
   `Raster.lock` has not been rebuilt). Rather than reverse that, `--no-auth` **degrades**: an
   unresolvable identity drops the chain-commitment and the run proceeds. Authenticated still
   errors. The dev loop keeps working and the cheap posture still produces a commitment whenever
   it can.

2. **The interim execution-fraud path (§4b of the plan).** Dropping the checkpoint field deletes
   what the chain-fraud guest's `Execution` variant asserted against, and §3 is not built, so
   removing the variant outright would leave *no* execution-fraud path at all. Instead
   `chain fraud-prove` emits a **terminal-window receipt** over the auditor's own honest re-run:
   *honest execution of this checkpoint's program, on its committed inputs, ends in a different
   output.* It is labelled in the CLI as **evidence, not a self-contained fraud proof** — nothing
   in it shows the trace it was proven over is the honest one, which is what §3's timeout
   supplies. For a local auditor, proving and checking being the same party, that gap costs
   nothing, and the artifact is exactly what §3 will open a challenge with.

**Known limitation.** Both the evidence receipt and (later) any challenge need a terminal window,
and a window opening inside a live recur loop is rejected today — so a recur-heavy stage cannot be
disputed until [`window-seed-reconstruction.md`](./window-seed-reconstruction.md) lands. This is
attack **S3**, and it is now load-bearing rather than theoretical.

## Verification

- **Unit** — terminality pin: a window ending at `fingerprint_len` yields `Established { X }`; one
  ending earlier is rejected even when it contains a `ProgramEnd`. `Next` continuity rejects a
  relabelled output payload.
- **Admission** — each of H3, H4, H5, H6, H9, H10 as a rejected `StageChallenge`, asserting the
  specific check that fired.
- **H1, the decisive one** — build a challenge from the claimer's *own honest* `commit.bin` with a
  fabricated `O'`; assert it cannot be constructed (the receipt's `Established` carries the honest
  output, so the `O' != s.output_payload_commitment` check fails). This is the test that
  distinguishes this design from the counter-checkpoint.
- **C1, end to end** — on `examples/chain-example`, tamper stage 2's `output.bin`, run
  `chain challenge`, assert the claimer's refutation attempt fails and the timeout condemns.
- **H2, end to end** — publish a fabricated `T_C`; assert the claimer refutes and a
  `ChallengeFraudJournal` verifies.
- **Equivalence** — a chain run cheaply and a chain run authenticated produce **identical
  `ChainCommitment` bytes**. This is the claim the whole proposal rests on and it extends
  `chain-stage-execution.md`'s byte-identical-`output.bin` test one level up.
- **Cost** — report the `hello-tiles` and `chain-example` deltas against the 6.6× baseline, and
  the wall clock of a full challenge/response cycle.
- **S3 regression** — a recur-heavy stage whose terminal window opens mid-loop: assert the
  challenge is *inexpressible* until `window-seed-reconstruction` lands, so the dependency is a
  failing test rather than a paragraph.

## Open questions

1. **What the DA admission rule looks like** (H7, §Assumed infrastructure 2). Not *whether* — the
   DA layer is planned and the header already content-addresses the body. The open part is where
   the retrievability check sits and what it costs the challenger: a deposit-and-serve window, a
   DA-layer inclusion proof at admission, or a claimer-initiated "unavailable" objection that
   pauses `T`.
2. **Is the terminal proof actually marginal?** The §3-versus-cheap-challenge trade turns on the
   terminal window proof being small against an authenticated stage re-run plus a DA publication.
   That is an assumption, not a measurement. Measure it on a real stage before the protocol
   objects are frozen; §Alternatives records what flips if it is wrong.
3. **Should a refuted challenge close the dispute permanently?** It must not — a refutation proves
   *that* challenge wrong, not the chain right. Per-challenger bonds, standard optimistic
   semantics; recorded because the naive reading is the unsound one.
4. **`BITS_PER_ITEM_FLOOR`'s value.** A floor, or a requirement that `bits_packer` match
   `FraudProofConfig::from_window_size(w)` for some legal `w`? The latter is tighter and no harder.
5. **Bond sizing against the asymmetry this design leaves.** Accusation costs a stage re-run + DA
   + one proof; defence costs one proof. The bond should price the residual gap, not the whole
   accusation, or honest challenging becomes uneconomic.

## Out of scope

- Settlement: contracts, calldata, bonds, clocks, and the DA layer itself. Assumed, not designed
  — §Assumed infrastructure states what is relied on and §Attack surface marks what depends on
  each.
- DAG chains, multi-fault attribution, parallel challenges.
- `chain_spec_commitment` (S1) — carried forward from `chain-fraud-proof.md` §Future work.
- The `chains-dry/` rename and the `dry-run` naming question — `chain-stage-execution.md` §1.

## Future work

- **Intra-stage segment anchors** (§Alternatives) — the next granularity down, for when one
  stage's dispute-time re-run is itself the bottleneck.
- **The cheap-challenge flip** (Open question 2), once bonds exist.
- **The dual** — folding honest per-stage receipts into one succinct validity proof, which would
  make the silence-based branch unnecessary.
- **A chain-root `Raster.lock`** pinning each stage's `program_commitment`, the anchored
  expectation an in-proof `Identity` fault needs — unchanged from `chain-fraud-proof.md`.
