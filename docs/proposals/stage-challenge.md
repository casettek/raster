# Proposal: `stage-challenge` — a cheap stage challenge, disproved in the zkVM by its defence

Status: **Phases 1–3 implemented** (Phases 1–2 on 2026-10-28, Phase 3 on 2026-10-29). Landed:
`TransitionState::TerminalMatch`; the host `WindowProver`; both forms of challenge (`chain
challenge`, `chain challenge-verify`); and the defence (`chain defend`, `chain defend-verify`, the
`defence` guest), with the trace's end tied to the commitment's. Phase 4 (the timeout) is
specified, not built, and deferred to [`settlement-layer.md`](./settlement-layer.md). See
§Implementation status.

Parties: the **claimer** publishes a chain commitment, a **challenger** challenges it, a
**defender** — the claimer, or anyone — defends against a disputed challenge.

Builds [`chain-io-commitment.md`](./chain-io-commitment.md) §3's dispute, with two **labelled
disagreements**:
- **§3's admission rule.** chain-io has the challenger attach a terminal-window proof. Here the
  challenge carries **no proof**: it is I/O plus a trace commitment, and all zkVM work happens in
  the *defence*. This is the rule chain-io recorded as "cheap challenge, claimer disproves"
  (§Alternatives there) and rejected on the margin; §Why the cheap rule says what changed.
- **§2's "`finalize` is untouched".** The defence that disproves a mislabelled challenge is a
  terminal window that *matches* its commitment, and the guest could not prove one. Phase 1 fixed
  that.

Everything else chain-io landed — I/O-only checkpoints, `OutputAuthorization::Established
{ output_commitment }`, `window_is_terminal`, `TraceVerifier::terminal_window`,
`detect_output_fraud`, authenticated `chain run --stage` — is kept and used as it is.

Builds on:
- [`chain-fraud-proof.md`](./chain-fraud-proof.md) (implemented) — the window/commitment binding
  and the image-id pinning discipline.
- [`window-seed-reconstruction.md`](./window-seed-reconstruction.md) (implemented 2026-09-09) — a
  terminal window may open inside a live recur loop, so every stage's challenge can be defended.
  This is what chain-io called attack **S3**, now closed.
- [`trace-end-windows.md`](./trace-end-windows.md) (implemented 2026-09-16) — revealed tail roots,
  so a defence can also refute the last items of a challenger's trace.

## The protocol

A challenge takes one of two forms, fixed by the fault it alleges:

- **Proven** — the commitment contradicts itself: a `Link` fault (a stage's committed input is
  not its producer's committed output) or a `Shape` fault (a repeat count its producing stage did
  not commit). Visible from the commitment alone, with no execution, so the challenger proves it
  on the spot with the chain-fraud guest and posts the receipt. There is nothing to defend:
  verifying it condemns the chain, with no clock. (This is what `chain fraud-prove` did; it is
  now the proven form of `chain challenge`.)
- **Disputed** — the commitment is self-consistent but a stage's output is wrong. Only execution
  shows that, and proving a whole stage is what fraud windows exist to avoid, so it is the game
  below.

The game for a disputed challenge:

1. **The claimer** publishes a `ChainCommitment` of I/O-only `StageCheckpoint`s: per stage its
   program, its authorized inputs and its output `O`. No trace.
2. **A challenger** re-runs the stages (cheaply, unauthenticated) and finds the first stage *i*
   whose output differs from the claim. It re-runs stage *i* authenticated (`chain run --stage`),
   which gives its own trace and commitment `H_C`.
3. **The challenger posts a challenge**: stage *i*, `H_C`'s header (its body made available), and
   the output `O'` it claims `H_C` ends in, with `O' ≠ O`. **No proof** — the challenger runs no
   zkVM.
4. **Anyone may defend** within a response window `T` — the claimer has the incentive, but an
   defence needs only public data. A defence is one zkVM proof over `H_C`, of one of two kinds:
   - **refutation** — `H_C` is not an honest execution of stage *i*: a fraud window, `Finished`;
   - **mislabel** — `H_C` is honest but does not end in `O'`: its terminal window, `TerminalMatch`,
     with an output `X ≠ O'`.
5. **Outcome.** A verified defence dismisses the challenge. No defence before `T` expires condemns
   the chain, stage *i* to blame — the claimer is the fraudulent party.

The two defences cover every challenge (§Soundness): if `H_C` diverges from honest execution
anywhere, a refutation exists; if it does not, its terminal window matches, and its real output
`X` either differs from `O'` (mislabel) or equals it — in which case `O' = X ≠ O` is the honest
output and the claimer lied, so no defence exists and none should.

## Why the cheap rule

chain-io kept the proof with the challenger for three properties: a mislabelled `O'` cannot be
posted (**H1**), a fabricated commitment cannot be admitted (**H12**), and an offline honest
claimer is condemned only by an accusation that proves something. It costs the challenger one
terminal-window proof per challenge, which it judged marginal beside the authenticated stage re-run
the challenger pays anyway.

This proposal takes the other side:
- **The challenger runs no zkVM.** Challenging needs only the stage re-run and data
  availability, so detection — the step that keeps a chain honest — has the lowest possible cost
  and needs no prover.
- **The zkVM runs only when someone disputes.** An honest stage under a false challenge costs one
  proof, paid by whoever defends; a true challenge costs no proof at all.
- **What is given up is priced instead of prevented.** H1 and H12 become admissible challenges
  that a defence disproves (a mislabel proof, a refutation). Their cost to the defender is priced
  by the challenger's bond [infra], forfeited when a defence dismisses the challenge.
- **The offline-claimer weakness is narrowed.** Defences are computed from public data, so any
  honest watcher can defend on the claimer's behalf; an honest stage is condemned only if
  *nobody* defends within `T`.

chain-io records the condition that flips the choice back: if the terminal proof is *not*
marginal against the stage re-run, the cheap rule is the cheaper one. Measuring it on a real stage
(iris `prefill_range`) remains open question 1; the guest work is the same under both rules, so
switching back changes only admission and the defence types.

## Problem

Steps 1–2 exist. Steps 3–5 have no code, and the mislabel defence could not be proven at all.

**The mislabel defence failed in the guest.** It is the terminal window of the challenger's own
honest trace, proven against the challenger's own commitment: every item matches. The transition
guest knew one way to end a window — in `finalize` (`guests/transition/src/fraud_proof.rs`),
every item before the last must match and **the last must diverge**, by fingerprint entry or by
revealed root — and a matching last item panicked:

```text
A fraud proof's final step must diverge from the commitment, by fingerprint entry or by
revealed trace root
```

Observed end to end on 2026-10-27 through `chain fraud-prove` (whose `prove_terminal_window`
proved exactly this window; both since folded into `chain challenge`): a one-stage chain, stage output
tampered in the chain commitment, dev mode. Every step of the terminal window verified, including
`ProgramEnd`'s output check, and the guest panicked in `finalize`. The requirement predates that
path (it is in `8e3daad`'s guest), so the receipt had never been producible. Phase 1 fixed it.

**The defence is unbuilt.** Phase 2 added both forms of challenge and their verification
(`Challenge`, `StageChallenge`, `verify`, `chain challenge`, `chain challenge-verify`); there is no
defence command and no defence journal yet (`grep` finds no `DefenceJournal` or `defend` in
`crates/`). chain-io's
status lists §3 as "not built — inert without a settlement clock". The clock is still assumed
infrastructure; the objects and the decision rule on it are not, and can be built and tested
without it.

## Goal

1. The transition guest can prove a **terminal attestation**: a terminal window that matches the
   commitment it names, ending in a guest-established output. *(Phase 1, done.)*
2. A terminal attestation can never be read as a fraud proof, by construction rather than by
   every consumer remembering to check. *(Phase 1, done.)*
3. A disputed challenge is cheap and proof-free; its admission is a pure check over public data.
   A proven challenge carries its receipt and condemns on verification. One command posts either.
4. Both defences exist as commands, are aggregated into one verifiable dismissal, and can be
   produced by anyone; the timeout is the one piece left to settlement.

## Facts the design builds on (verified in code, 2026-10-27)

| # | Fact | Where |
|---|---|---|
| G1 | `TransitionState` was `Init`, `Next`, `Finished`; the guest refuses to continue a `Finished` state | `raster-core/src/transition.rs:336`; `fraud_proof.rs:267` |
| G2 | `finalize` ended a window only on divergence (fingerprint entry, or `final_committed_root` in the revealed tail) | `fraud_proof.rs`, `pub fn finalize` |
| G3 | `window_is_terminal` is derived in-guest at `Init` — `window_start + window_len == fingerprint_len` — and inherited across `Next` | `fraud_proof.rs:475` |
| G4 | The step is verified (`apply_verified_step`, including `verify_program_end`) and its output authorization taken **before** `finalize` | `guests/transition/src/main.rs` |
| G5 | `verify_program_end` returns `OutputAuthorization::Established { output_commitment }`, the value it checked | `checks/entrypoint.rs:228`, `:314` |
| G6 | The only readers of `Finished` outside the guest treat it as *fraud proven*; none matches other states as fraud | `examples/verify_fraud_proof.rs:33` |
| G7 | Authenticated `chain run --stage` writes the stage's `commit.bin` and leaves the chain commitment alone | `raster-cli/src/main.rs` (`stage` doc) |
| G8 | `detect_output_fraud` finds output fraud cheaply (unauthenticated re-runs); the then `chain fraud-prove` built a commitment from an authenticated re-run | `raster-cli/src/chain.rs`, `detect_output_fraud` |
| G9 | The chain-fraud guest proves `Link` and `Shape` faults only; `Execution` was removed with the checkpoint's trace field | `raster-core/src/chain.rs:140` |
| G10 | `TraceVerifier::verify` against a named commitment yields the fraud window a refutation needs; `TraceVerifier::terminal_window` the window a mislabel proof needs | `raster-prover/src/trace.rs` |

## Design

### Phase 1 — the guest: a second way to end, kept apart from the first *(implemented)*

One state, appended after `Finished` so existing encodings are unchanged:

```rust
pub enum TransitionState {
    Init(InitTransition),
    Next(Transition),
    /// The last item diverged from the commitment: fraud proven.
    Finished,
    /// A terminal window whose every item matched the commitment: honest
    /// execution of this window ends the trace, with the journal's
    /// `output_authorization`. An attestation, never a fraud proof.
    TerminalMatch,
}
```

`finalize`, on the window's last item, through `fraud_proof::window_end`:

```rust
if diverges || diverges_by_root {
    TransitionState::Finished
} else if window_is_terminal {
    TransitionState::TerminalMatch
} else {
    panic!("A fraud proof's final step must diverge from the commitment, …");
}
```

`finalize` takes `window_is_terminal` from the window context, which already holds it (G3). The
guest refuses to continue `TerminalMatch` exactly as it refuses `Finished` (G1).

Why a separate state rather than `Finished` plus a journal flag: every existing consumer that
means *fraud proven* tests for `Finished` (G6). A new state fails that test without any consumer
changing; a flag would need every consumer to learn to check it, and the one that forgets turns an
honest attestation into a "fraud proof". The rule is enforced by the type.

What a `TerminalMatch` journal states, every part guest-derived: the commitment
`refuted_trace_commitment` (here, the attested one) ends in this window (`window_is_terminal`);
every window item matched it; the items are an honest execution of `program_commitment` on
`input_manifest_commitment` (`entrypoint_authorization` established at the window's opening,
chain-io F7); and the trace's output is `output_authorization = Established { X }` (or
`NotRequired` for a unit-output program). It says nothing about the commitment's items *before*
the window — a refutation covers those.

### Phase 2 — the window prover, and the challenge

#### The host's window prover

The defences add two callers of the proving pipeline. Each needs the same operation: *prove
window W of trace T against commitment C, and say how it ended*. Before Phase 2 two callers — `run
--audit` and `chain fraud-prove` — call `commands::prove::prove` (which builds witnesses and
drives `step_transitions`) and each re-reads the raw journal to learn what it got. Before adding
more, put one API in front of that:

```rust
/// How a proven window ended — read from the journal the guest committed, never assumed.
pub enum WindowOutcome {
    /// The last item diverged: a fraud proof against `commitment`.
    Diverged,
    /// A terminal window that matched throughout: an attestation of the trace's honest end.
    TerminalMatch,
}

pub struct WindowProof {
    pub receipt: Receipt,
    pub journal: TransitionJournal,
    pub outcome: WindowOutcome,
}

/// Proves windows of one trace against one named commitment.
pub struct WindowProver<'a> {
    trace: &'a Trace,
    commitment: &'a TraceCommitment,
    cfs: &'a ControlFlowSchema,
    recorder: &'a TraceRecorder,
    replayer: &'a Replayer<'a>,
    input_manifest: Option<&'a str>,
}

impl WindowProver<'_> {
    /// The fraud window a `TraceVerifier::verify` divergence names.
    /// `run --audit`, and a refutation; errors unless the outcome is `Diverged`.
    pub fn prove_divergence(&self, evidence: FraudEvidence) -> Result<WindowProof>;

    /// The commitment's terminal window (`TraceVerifier::terminal_window`).
    /// A mislabel proof; errors unless the outcome is `TerminalMatch`.
    pub fn prove_terminal(&self) -> Result<WindowProof>;
}
```

- **The outcome is read, not assumed.** `WindowProof::outcome` is derived from
  `journal.current_state` (`Finished` → `Diverged`, `TerminalMatch` → `TerminalMatch`), and each
  method errors when the guest ended the window otherwise. A caller cannot submit an attestation
  where it meant a fraud proof, or the reverse — the check Phase 1 added by hand to the then
  `chain fraud-prove` became the API's.
- **It binds the commitment once.** Construction records `commitment.header().digest()`, and
  every `WindowProof` is checked to carry it as `refuted_trace_commitment`.
- **Where it lives.** `raster-cli`, beside `commands::prove`, since it needs the CLI's `Replayer`
  and `TraceRecorder`; `commands::prove::prove` becomes its private implementation.
- **It survives the prover split.** On `prover-split/fraud-window` the same entry point,
  `commands::prove::prove`, drives the FRAUD WINDOW pipeline (`raster-prover/src/fraud_window.rs`,
  `prove_fraud_window`, returning a `FraudWindowJournal`). Only `WindowProof`'s journal type and
  the source of `outcome` change there; no caller does. See §Merging with the prover split.

The guest keeps its shape on `main`: `FraudProofWindowContext` and `finalize` already are the
window's open/step/end, and the prover split reorganizes exactly that, so a guest-side
abstraction here would be replaced at merge. A local tidy — the window context ending the window
itself rather than handing `finalize` its fields — is fine whenever convenient.

#### The challenge: two forms, one object

```rust
pub enum Challenge {
    /// A `Link` or `Shape` fault, proven: verifying it condemns the chain.
    Proven(ProvenChallenge),
    /// A stage's output, disputed without proof: defended, or upheld at the timeout.
    Disputed(StageChallenge),
}

pub struct ProvenChallenge {
    /// The chain under challenge, whole: the receipt's chain digest is checked against it.
    pub chain_commitment_bytes: Vec<u8>,
    /// The chain-fraud receipt (`ChainFraudJournal`: chain digest, faulty stage, fault kind).
    pub receipt: Receipt,
}

pub enum Verdict {
    Condemned { chain, chain_digest, journal: ChainFraudJournal },
    AwaitingDefence(AdmittedChallenge),
}

pub fn verify(challenge: &Challenge) -> Result<Verdict, AdmissionRefusal>;
```

A proven challenge verifies when its receipt is a chain-fraud receipt (pinned image id, and the
known-good inner guests — `verify_chain_fraud_receipt`) and condemns the chain the challenge
carries: `journal.chain_commitment_digest == sha256(chain_commitment_bytes)`. Both forms name
their chain by the digest of the bytes they carry, as the chain-fraud guest computes it.

`Challenge`, `ProvenChallenge`, `Verdict` and `verify` live in `raster-prover`
(`src/challenge.rs`): a receipt is a host type. `StageChallenge` lives in `raster-core`, because
the defence's aggregation guest will decode it.

The disputed form:

```rust
pub struct StageChallenge {
    /// The chain commitment under challenge, whole: admission recomputes its digest.
    pub chain_commitment_bytes: Vec<u8>,
    pub challenged_stage: u32,
    /// `H_C`'s header; its body is published alongside for availability.
    pub trace_commitment_header: TraceCommitmentHeader,
    /// The output the challenger claims `H_C` ends in — an assertion, not a proof.
    pub claimed_output: Vec<u8>,
}
```

**Admission** — a pure check over public data, no proving. With `s = stages[challenged_stage]`:

```text
challenged_stage < stages.len()
s.output_payload_commitment non-empty                     -- a unit-output stage has no output to dispute
claimed_output               != s.output_payload_commitment
header.window_size == protocol_window_size(header.fingerprint_len)   -- decision 6: 128, capped below the length
header.bits_packer  == FraudProofConfig::from_window_size(window_size).bits_packer      -- decision 1, chain-io H5
H_C's body retrievable                                     -- [infra]
```

Admission of the disputed form is a pure function, `verify_challenge`, run by anyone (and later by a settlement
contract). It lives in `raster-prover` (`src/challenge.rs`), not `raster-core` beside
`StageChallenge`, because the canonical bits per item come from `FraudProofConfig`. It deliberately checks nothing about `H_C`'s *contents*: whether the trace is honest,
and whether it ends in `claimed_output`, is exactly what a defence disproves.

CLI:
- `chain challenge [CHAIN] [CHAIN_COMMITMENT] [--stage NAME]` —
  loads the chain as claimed and looks for a fault in order: `Shape`, `Link` (each proven with
  the chain-fraud guest → a **proven** challenge), then output fraud (the first stage whose
  unauthenticated re-run differs from its committed output). For that, it re-loads the chain as a
  verifier would (refusing a shape invalid on its face), re-runs the stage authenticated, builds
  `H_C` at the protocol window for its trace length (decision 6), and writes a **disputed** challenge with `claimed_output` read from its own
  `output.bin` — no proving. `--stage` skips the search and disputes that stage. Either form goes
  to `<run>/challenge/challenge.bin`, a disputed one with `H_C`'s `commit.bin` beside it; the
  challenge is verified first, and one that would be refused is never written.
- `chain challenge-verify <challenge> [--chain-commitment PATH]` — `verify`: a proven challenge
  reports the chain condemned (stage, fault kind); a disputed one is admitted, plus local
  availability — `commit.bin` beside it must decode and be the challenge's `H_C` — and awaits a
  defence. With `--chain-commitment`, also that the challenge is against that chain.

`chain fraud-prove` and `chain fraud-verify` are gone: their `Shape`/`Link` receipts are the proven
form, and their output-fraud evidence — a `TerminalMatch` receipt over the auditor's own re-run,
which proves nothing about the claimer — is replaced by the disputed form.

### Phase 3 — the defence

`chain defend <challenge>`, run by the claimer or by anyone:

1. Admit the challenge (`verify_challenge`); refuse to defend an inadmissible one.
2. Re-run stage *i* authenticated and walk the honest trace against `H_C`'s published body
   (`TraceVerifier::verify`, G10).
3. **A divergence** yields a fraud window: `WindowProver::prove_divergence` over `H_C` →
   `Finished`. This is a **refutation**.
4. **No divergence** means `H_C` is honest. Prove its terminal window,
   `WindowProver::prove_terminal` over `H_C` → `TerminalMatch` with `Established { X }`.
   - `X ≠ claimed_output` → this is a **mislabel proof**.
   - `X == claimed_output` → **no defence exists**: `H_C` is honest and ends where the challenger
     said, so the claimed output is false. `chain defend` reports this rather than proving it.
5. Aggregate the defence into a `DefenceJournal` (in its own guest — §Phase 3 plan, change 2):
   *this challenge is fraudulent*, by one of two checks, after the common ones:

```text
common      verify(transition_image_id, receipt)
            j.refuted_trace_commitment   == digest(challenge.trace_commitment_header)
            j.program_commitment         == s.program_commitment
            j.input_manifest_commitment  == s.input_manifest_commitment
            chain digest recomputed from challenge.chain_commitment_bytes
refutation  j.current_state == Finished
mislabel    j.current_state == TerminalMatch
            j.window_is_terminal
            j.entrypoint_authorization ∈ { Established, NotRequired }
            j.output_authorization == Established { X },  X != challenge.claimed_output
```

The journal records which defence it was (`reason: Refuted | Mislabelled`). The program and
manifest equalities are what make `X` *this stage's* honest output, so a mislabel proof over some
other program's trace cannot dismiss the challenge.

CLI: `chain defend-verify <defence>` checks the aggregated receipt against the pinned defence
image id.

#### Phase 3 plan (2026-10-29)

Three changes to the design above, each labelled; then the steps.

**Change 1 — the trace's end is tied to the commitment's (open question 5).** Without it an
honest claimer has no defence against an `H_C` of the wrong length. Confirmed in the code, not yet
reproduced: for a *padded* `H_C`, `TraceVerifier::verify` returns `Ok`, and the window
`terminal_window` builds over the honest trace is not `H_C`'s terminal window, so the guest
refuses it. For a *truncated* `H_C`, `H_C`'s terminal window does not end in `ProgramEnd`: it
ends `TerminalMatch` with `output_authorization = Pending`, which no mislabel check accepts.
`window_end` gains the step's kind, and the two length faults become refutations:

```text
diverges                                  Finished        (unchanged)
matches, terminal,     ends in ProgramEnd TerminalMatch   (unchanged)
matches, terminal,     not ProgramEnd     Finished        H_C ends before the program does
matches, not terminal, ends in ProgramEnd Finished        H_C goes on after the program ended
matches, not terminal, not ProgramEnd     refused         (unchanged)
```

This is sound because an honest commitment always ends in `ProgramEnd`, at its last index and
nowhere else. `ProgramEnd` is the unique terminal step: the guest lets nothing follow it, and a
`main` that fails produces no `ProgramEnd` (`trace.rs`, `ProgramEndStep`). So neither new case can
fire against an honest commitment of a complete run. `TraceVerifier::verify` yields both windows.
If the honest walk reaches `H_C`'s length with items still to come, it yields `H_C`'s terminal
window. If the walk ends before `H_C` does, it yields the window ending at the honest
`ProgramEnd`. One consequence: a `Finished` window means "`H_C` is not an honest, complete
execution", which is the claim a refutation needs.

**Change 2 — the defence aggregates in its own guest, not in the chain-fraud guest.** One
image id would then commit two journal types (`ChainFraudJournal`, `DefenceJournal`), and a
consumer decoding the wrong one is the same fail-open hazard Phase 1 avoided with a separate
state. A `defence` guest (`guests/defence`, `DEFENCE_GUEST_ID`) commits only `DefenceJournal`. As a
side effect the chain-fraud image id does not change, so posted `Link`/`Shape` receipts stay valid.

**Change 3 — the defence's kind is read from the journal, never offered.** The guest derives
`reason` from `j.current_state`: `Finished` gives `Refuted`, `TerminalMatch` gives `Mislabelled`,
and any other state is refused. A `TerminalMatch` offered as a refutation therefore cannot be
expressed at all. The wrong-defence tests become: a mislabel with `X == claimed_output`, one whose
output is not `Established`, a non-terminal mislabel, and any defence over another commitment,
program or manifest.

`DefenceJournal`, all of it computed or checked in the guest:

```rust
pub struct DefenceJournal {
    pub challenge_digest: [u8; 32],        // sha256(postcard(StageChallenge)) — which challenge
    pub chain_commitment_digest: [u8; 32], // sha256(challenge.chain_commitment_bytes)
    pub challenged_stage: u32,
    pub trace_commitment: Vec<u8>,         // digest(H_C's header)
    pub reason: DefenceReason,             // Refuted | Mislabelled
    pub attested_output: Option<Vec<u8>>,  // X, for Mislabelled
    pub transition_image_id: Vec<u8>,      // checked against the known-good id by the verifier
    pub authorization_image_id: Vec<u8>,   // likewise: the window's manifest is only as good as it
}
```

The checks are one pure function in `raster-core`, `chain::check_defence`, called by the
guest and by `chain defend` before it proves. The guest and the host therefore cannot disagree,
and the wrong-defence cases are unit tests that need no zkVM.

**Steps**

1. **Guest ending rule** (Change 1). `window_end(diverges, window_is_terminal, ends_program)`;
   `main.rs` passes the step's kind; `window_end` unit tests cover all five rows.
2. **Host windows for length faults.** `TraceVerifier::verify` yields the two windows above.
   The divergence window builder is factored out and shared, and `trace.rs` unit tests cover a
   padded and a truncated commitment.
3. **Defence types** (`raster-core/src/chain.rs`): `DefenceInput`, `DefenceJournal`,
   `DefenceReason`, `DefenceRefusal`, `check_defence`, and unit tests for every common and
   per-reason check.
4. **Defence guest and host driver.** `guests/defence` (in `[package.metadata.risc0]`) calls
   `env::verify` on the transition journal, then `check_defence`, and commits.
   `raster-prover/src/defence.rs` has `prove_defence` and `verify_defence`: the pinned
   `DEFENCE_GUEST_ID`, then the committed transition and authorization image ids against the
   known-good ones.
5. **CLI.** `chain defend [CHAIN] [CHAIN_COMMITMENT] [--challenge PATH]`:
   - Admit the challenge, and load the chain the challenge carries (refusing a mismatched run
     directory).
   - Read `H_C`'s body from beside the challenge, and re-run the stage authenticated into
     `<stage>/defence/`.
   - Walk the honest trace against `H_C`:
     - **divergence:** `prove_divergence`, giving `Refuted`;
     - **no divergence, honest output == `claimed_output`:** report that no defence exists, with
       no proving;
     - **otherwise:** `prove_terminal`, giving `Mislabelled`.
   - Aggregate and write `<run>/challenge/defence.bin`.

   `chain defend-verify <defence> [--challenge PATH]` runs `verify_defence`, plus that the
   defence answers the challenge beside it (by `challenge_digest`).
6. **Adversarial challenges for the end-to-end checks.** `chain challenge --stage S --forge K`,
   a hidden, test-only flag, posts a false challenge. It disputes the honest output with a flipped
   one, over `H_C` built from the honest trace with one change, chosen by `K`:
   - `claimed-output`: no change; the honest `H_C`, mislabelled (H1);
   - `item`: one item replaced by its predecessor (H12);
   - `pad`: one item appended after `ProgramEnd`;
   - `truncate`: the last item dropped.

   `H_C` is committed at the protocol window for its own length, so each forged challenge stays
   admissible. It is a flag rather than the planned `raster-prover` example, because forging
   needs the full honest trace, and only the CLI can load one (through `raster-runtime`'s
   recorder, which depends on `raster-prover`). It forges nothing a challenger could not: the
   protocol must survive any challenge bytes.
7. **End to end, in dev mode** on `chain-example`, with stage `normalize`'s output tampered in the
   chain commitment: a true challenge gets no defence, and the four tampered challenges are each
   defended and `defend-verify`'d. Then the honest-chain byte-identity check.
8. **Docs.** Implementation status here; chain-io's status (§3's admission rule replaced); open
   question 5 closed.

Every guest edit changes image ids. Rebuild `cargo-raster` and the `verify_fraud_proof` example
together, and re-lock `chain-example`'s stages before the end-to-end run (raster-core changes in
step 3 can move tile ids).

### Phase 4 — the timeout, specified not built

Moved to its own deferred proposal, [`settlement-layer.md`](./settlement-layer.md) (2026-10-29). It
covers:
- the decision rule as a state machine;
- `T`, sized from the measured defence cost;
- bonds and data availability, including the defender's public bundle;
- receipt verification where the settlement runs;
- defender liveness, which depends on open question 6.

The summary below stays for context.

The decision rule, for a settlement layer to implement: an admitted challenge with no verified
`DefenceJournal` before `T` condemns the chain, `challenged_stage` to blame; a verified one
dismisses the challenge and forfeits the challenger's bond. Clock, bonds and data availability of
`H_C`'s body stay [infra], as chain-io §Assumed infrastructure records. Under the cheap rule the
challenger's bond is no longer optional: it is what prices a false challenge (§What each party
can and cannot do).

### What each party can and cannot do

| Situation | Defence | Result |
|---|---|---|
| Claimer's commitment contradicts itself (`Link`, `Shape`) | none possible — a proven challenge | condemned on verification, no clock |
| Claimer lied about stage *i*'s output; challenger's `H_C` honest, `claimed_output` true | none exists: `H_C` honest, ends in `claimed_output` | condemned at `T` |
| Claimer honest; challenger posts the honest trace with an invented `claimed_output` (H1) | **mislabel**: `H_C`'s terminal window ends in `X = O ≠ claimed_output` | dismissed, challenger's bond forfeited |
| Claimer honest; `H_C` fabricated anywhere, or garbage (H12) | **refutation**: the walk diverges | dismissed, bond forfeited |
| Claimer honest; `claimed_output == O` | — | inadmissible |
| Claimer honest but offline | any watcher defends from public data | dismissed |
| Defence is a `TerminalMatch` offered as a refutation, or a `Finished` as a mislabel | — | rejected by the aggregation |
| Mislabel proof whose `X == claimed_output` | — | rejected |
| Defence over a different commitment, program or inputs | — | rejected by the common checks |
| `TerminalMatch` receipt submitted anywhere as a fraud proof | — | rejected by every existing consumer, which requires `Finished` |

## Soundness

- **Completeness of the defences.** For an admitted challenge, exactly one of these holds. `H_C`
  diverges from the honest execution of stage *i* at some item: the walk finds it, and a
  refutation proves it (detection under the existing fingerprint margin, exact roots in the
  revealed tail). Or `H_C` is the honest trace: its terminal window matches, and the mislabel
  proof's `X` is the honest output. Then `X ≠ claimed_output` dismisses the challenge, and
  `X = claimed_output` means `claimed_output` is the honest output and differs from the claim — the
  claimer is wrong, and no defence exists.
- **No honest claimer is condemned while anyone defends.** An honest claimer's output is the
  honest output `O`. A challenge against it has `claimed_output ≠ O`, so either `H_C` is dishonest
  (refutation) or it is honest and ends in `O ≠ claimed_output` (mislabel). A defence always
  exists, from public data alone.
- **No dishonest claimer is saved.** Against a true challenge, a refutation would need `H_C` to
  diverge from honest execution, and a mislabel proof would need its honest end to differ from
  `claimed_output`; neither holds. The defence's checks bind it to `H_C` and to stage *i*'s program
  and inputs, so a receipt over anything else does not count.
- **Separation of the two receipts** (Phase 1). `Finished` and `TerminalMatch` are distinct states
  from disjoint conditions in one function, committed in the journal, so no host can relabel one
  as the other; the aggregation checks the state each defence type requires.
- **The fraud rule is unchanged.** A non-terminal window still must diverge at its last item, and
  a diverging window still ends `Finished`. Single-program audit and the refutation use today's
  path.

What the cheap rule relies on that chain-io's did not: **liveness of at least one honest
defender** within `T`, and **the challenger's bond** to price false challenges. Both are
[infra]; neither is a soundness assumption of the proofs themselves.

## What this breaks

- `TransitionState` gains a variant *(landed)*: the transition guest's image id changes, and so
  does every journal encoding that matches on the enum exhaustively in code (compile errors, not
  silent).
- `window_end` ties the trace's end to the commitment's (Phase 3, change 1): the transition
  guest's image id changes again. A non-terminal window ending in `ProgramEnd`, and a terminal one
  that does not, now end `Finished` instead of being refused or ending `TerminalMatch`. Neither
  can occur against an honest commitment of a complete run.
- The defence aggregation is a new guest (`guests/defence`, `DEFENCE_GUEST_ID`), not a mode of the
  chain-fraud guest (Phase 3, change 2). The chain-fraud guest and its image id are unchanged.
- No `ChainCommitment`, `StageCheckpoint` or `commit.bin` changes. Honest chains and single-program
  commitments are byte-identical before and after.

## Merging with the prover split (`prover-split/fraud-window`)

That branch moves the window out of the single transition guest: a **trace** part guest binds
the window to the commitment and decides how it ends, and a **FRAUD WINDOW** aggregator stitches
the parts into one `FraudWindowJournal` (`raster-core/src/fraud_window.rs`, `check_window`).

**It has the same defect Phase 1 fixed.** Its trace part still requires every window's last item
to diverge — `guests/trace/src/segment.rs:83`, the same message — so a terminal window of an
honest trace panics there too, and its `chain fraud-prove` (`raster-cli/src/chain.rs:1285`) fails
the same way.

When the branch is brought onto `main`, Phase 1 moves with the window:
1. **The ending rule.** `window_end`'s three cases go into the trace part's segment end, where
   `segment.rs:83` decides today: diverged → the fraud ending; terminal and matching → the
   attestation; anything else refused. `window_is_terminal` is already derived in the trace part
   (`guests/trace/src/window.rs`).
2. **The journal.** `FraudWindowJournal` must say which ending happened, and with the same
   fail-closed rule: whatever a fraud consumer tests for today must not be satisfied by an
   attestation. If the journal has no state field of its own, add an outcome whose fraud value
   is the only one existing consumers accept, rather than a flag they would have to learn.
3. **Its consumers.** Anything on the branch that treats a `FraudWindowJournal` as fraud
   proven must check that outcome.
4. **The host.** `WindowProver`'s `outcome` is read from the `FraudWindowJournal`; its callers
   are unchanged.
5. **Tests.** Port the `window_end` unit tests to the trace part, and re-run the end-to-end
   defences (§Verification) on the merged pipeline.

Doing Phases 2–3 on `main` before the merge is fine: the protocol objects talk to `WindowProver`
and to journals' outcomes, not to the guest layout.

## Alternatives considered

- **The challenger proves (chain-io §3 as written).** The challenge carries a `TerminalMatch`
  receipt over `H_C` and admission checks it, so H1 and H12 cannot be posted and the only defence
  is a refutation. Sound, and strictly stronger against an absent defender, at one
  terminal-window proof per challenge and a prover in every challenger's hands. Not chosen; it is
  the fallback if bonds or defender liveness prove insufficient, and switching changes only
  admission and the defence types — Phase 1 and `WindowProver` serve both rules.
- **`Finished` plus a journal field `outcome: Diverged | TerminalMatch`.** Same information, but
  fail-open: a consumer that checks only `Finished` accepts an attestation as fraud. Rejected for
  the new state, which makes the safe reading the default.
- **Put the claimer's trace back in the checkpoint** (an earlier draft of this proposal). Makes
  execution fraud a self-contained receipt with no clock, but reverses chain-io's checkpoint
  narrowing and its cost argument (a claimable chain needs every stage authenticated, 6.6× on
  `hello-tiles`). Not what the protocol intends: the claimer commits I/O, and the trace appears
  only in a dispute. Withdrawn.

## Implementation plan

1. **Guest outcome** (Phase 1, *done*). `TransitionState::TerminalMatch`; `finalize` takes
   `window_is_terminal`; `proceed` refuses to continue it; guest unit tests; `chain fraud-prove`'s
   receipt completes.
2. **Window prover and challenge** (Phase 2, *done*). First `WindowProver` (`prove_divergence`,
   `prove_terminal`, `WindowOutcome`), with `run --audit` and `chain fraud-prove` moved onto it —
   no behaviour change, so the existing suites are its test. Then `StageChallenge`,
   `verify_challenge`, `chain challenge`, `chain challenge-verify`; then `fraud-prove`/
   `fraud-verify` folded in as the proven form (`Challenge`, `verify`).
3. **Defences** (Phase 3). `chain defend` with both branches, the chain-fraud aggregation and
   `DefenceJournal` (`reason: Refuted | Mislabelled`), `chain defend-verify`.
4. **Docs.** chain-io's status (§3's admission rule replaced, and its §4b interim evidence
   removed with `fraud-prove`; §2's `finalize` note already added). The proposals index and
   `ChainFaultKind`'s doc comment are done.

Phases 2–3 depend on Phase 1 and on nothing else unbuilt.

## Implementation status

**Phase 1 — landed 2026-10-28.**
- `TransitionState::TerminalMatch`, after `Finished` (`raster-core/src/transition.rs`).
- `finalize` takes `window_is_terminal` from the window context and delegates the decision to
  `fraud_proof::window_end`; `proceed` refuses to continue `TerminalMatch` as it refuses
  `Finished`.
- `chain fraud-prove` refuses to write a terminal-window receipt that did not end
  `TerminalMatch`, and reports the receipt's state and its guest-derived (attested) output.
  `examples/verify_fraud_proof.rs` names `TerminalMatch` as "NOT Finished".
- Tests: `window_end` unit tests in the transition guest (diverging → `Finished`, terminal and
  matching → `TerminalMatch`, matching and not terminal → refused); guest suite 166 passed,
  raster-core 202, raster-prover 40, raster-cli 122.
- End to end (dev mode), the 2026-10-27 scratch chain (`main` returning `Ok(call!(tile, …)?)`):
  honest chain — `chain fraud-prove` finds nothing; stage output tampered in the chain
  commitment — the terminal window, which panicked before, now ends `TerminalMatch` with
  `window_is_terminal = true` and the attested output equal to the honest one. That window is
  the mislabel proof's shape.

**Phase 2 — landed 2026-10-28.**
- `WindowProver` (`raster-cli/src/commands/window_prover.rs`): `prove_divergence`,
  `prove_terminal`, `WindowOutcome` read from the journal, and the commitment checked on every
  proof. `run --audit` (`commands::prove::generate_fraud_proof`) and `chain fraud-prove` moved
  onto it; both re-checked end to end in dev mode (`Fraud proof generated`; `TerminalMatch`).
- `StageChallenge` (`raster-core/src/chain.rs`); `verify_challenge` and `AdmissionRefusal`
  (`raster-prover/src/challenge.rs`), 7 unit tests: admitted; claimed == committed; unit-output
  stage; missing stage; illegal window size; bits per item one below and one above canonical;
  undecodable chain commitment.
- `chain challenge` and `chain challenge-verify`. End to end on the scratch chain (dev mode, no
  proving): honest chain — auto-detect reports nothing to challenge, `--stage greet` is refused
  (`OutputNotDisputed`); tampered chain — an admissible challenge at window 4, accepted by
  `challenge-verify`; `challenge-verify` refuses a missing body, a body that is not `H_C` (the
  stage's own window-2 `commit.bin`), and a file that is not a challenge.
- **`fraud-prove`/`fraud-verify` folded in** (same day): `Challenge { Proven, Disputed }`,
  `ProvenChallenge`, `Verdict`, `verify` (`raster-prover/src/challenge.rs`); `chain challenge`
  searches `Shape` → `Link` → output; one file, `<run>/challenge/challenge.bin`;
  `challenge-verify --chain-commitment`. The evidence-receipt path (`prove_terminal_window`,
  `execution-fraud.receipt`) is removed. Re-checked on the scratch chain: honest → nothing to
  challenge; tampered → a disputed challenge, admitted; refused against a different chain
  commitment. `chain_repeat_cli`'s shape tests now drive `chain challenge`/`challenge-verify`.
  Proven `Link` on `chain-example` (stage 1 `aggregate` blamed); a dev-mode receipt is refused
  with dev mode off.
- **Protocol window** (decision 6): `protocol_window_size`, admission refusals
  `WindowSizeNotProtocol` / `TraceTooShort` (replacing `IllegalWindowSize`); 10 admission unit
  tests. End to end: `greet` (7 items) → window 4; iris `prefill_range_l0` (2043 items) → 128.
- Known, pre-existing: a *relative* `CHAIN_COMMITMENT` path breaks the stage re-run (in
  `chain audit --execution` too) — the stage process resolves input paths from another directory.
  Absolute paths, or the default latest run, work.

**Phase 3 — landed 2026-10-29** (§Phase 3 plan; its three changes are labelled there).
- **Length binding** (change 1, open question 5): `window_end(diverges, window_is_terminal,
  ends_program)`. A terminal window not ending in `ProgramEnd`, and a non-terminal one that does,
  end `Finished`. `TraceVerifier::verify` yields both windows: when the walk outruns the commitment,
  and when it ends short of it. The divergence window builder is factored into `fraud_window`.
- **Defence types and check** (`raster-core/src/chain.rs`): `DefenceInput`, `DefenceJournal`,
  `DefenceReason`, `DefenceRefusal`, and `defend`, the one function both the guest and the host
  run.
- **Defence guest** (`guests/defence`, change 2): it calls `env::verify` on the transition journal
  against the committed transition image id, then `defend`, then commits. The host side
  (`raster-prover/src/defence.rs`) has `prove_defence`, which checks the host's `defend` before
  proving and compares the guest's journal with it, and `verify_defence`, which pins
  `DEFENCE_GUEST_ID` and then the known-good transition and authorization ids.
- **CLI**:
  - `chain defend [CHAIN] [CHAIN_COMMITMENT] [--challenge]`: admits the challenge, refuses a
    challenge against another chain, re-runs the stage into `<stage>/defence/`, and walks it
    against `H_C`. It then either proves a refutation, proves a mislabel, or reports that no
    defence exists (exit code 1, nothing proven), and writes `challenge/defence.bin`.
  - `chain defend-verify <defence> [--challenge]`: checks the receipt and the challenge digest.
  - `chain challenge --forge` (hidden, test only; plan step 6).
- **Tests**:
  - transition guest: 168 passed (`window_end`: all five rows);
  - raster-core: 207 passed (9 `defend` tests: both defences; the true challenge; output not
    established; not terminal; window not ended; another commitment, program or manifest for both
    defences; another transition guest; a missing stage);
  - raster-prover: 52 passed (a padded and a truncated commitment yield their windows);
  - raster-cli: 123 passed (84 unit, 39 integration including `chain_repeat_cli`; 1 ignored).
- **End to end** (dev mode, `chain-example`, stage `normalize`, a 14-item trace at window 8):
  - honest chain: `chain challenge` finds nothing;
  - `--forge claimed-output` (H1): defended as **mislabelled**, attesting the honest output
    `41f002…`;
  - `--forge item` (H12): **refuted**;
  - `--forge pad` (`H_C` of 15 items): **refuted**;
  - `--forge truncate` (13 items): **refuted**.

  The padded and truncated `H_C`s carry only honest items, so the length rule is the only thing
  that can refute them. Each defence passes `defend-verify`.
- **End to end, the refusals**:
  - True challenge (stage output tampered in the chain commitment): `chain defend` reports "no
    defence exists" and proves nothing.
  - A defence verified against another challenge is refused (challenge digest).
  - `chain defend` against a challenge on another chain is refused.
- `chain-example`'s three `Raster.lock`s were re-locked: every tile image id and program commitment
  moved, as raster-core edits do. There was no baseline re-lock, so whether this phase moved them is
  not established.

**Phase 4** — specified, not built; deferred to [`settlement-layer.md`](./settlement-layer.md).

## Verification

- **Proven challenge** *(done)*. A trip count tampered in the commitment gives a proven
  challenge that `challenge-verify` reports condemned, naming `Shape` (`chain_repeat_cli`,
  `a_shape_fault_produces_a_verifiable_receipt`). A malformed shape record is reported, not
  proven. A `Link` fault the same way, on `chain-example` (three stages).
- **Guest unit** *(done)*. A terminal window whose items all match ends `TerminalMatch` with
  `Established { O }`; with a diverging last item it ends `Finished`; a non-terminal matching
  window still panics; neither can be continued.
- **True challenge** *(done, 2026-10-29, on `chain-example`)*. One-stage chain, stage output tampered in the chain commitment (the
  2026-10-27 scratch case): `chain challenge` produces an admissible challenge with no proof;
  `chain defend` finds no divergence, proves the terminal window, finds `X == claimed_output`,
  and reports that no defence exists.
- **H1, mislabel** *(done)*. Honest chain; a challenge carrying the honest `H_C` and an invented
  `claimed_output` is admitted; `chain defend` produces a mislabel proof and `chain defend-verify`
  accepts it.
- **H12, refutation** *(done: one fabricated item; padded and truncated `H_C` too)*. Honest chain; a challenge whose `H_C` has one fabricated item (and,
  separately, random fingerprint bits) is admitted; `chain defend` refutes it.
- **Wrong defences** *(done, as `defend` unit tests; plus a defence against another challenge and another chain, end to end)*. A `TerminalMatch` offered as a refutation, a `Finished` offered as a mislabel,
  a mislabel with `X == claimed_output`, and a defence over another commitment, program or
  manifest are each rejected by the aggregation, asserting the check that fired.
- **Admission** *(done, unit tests)*. `claimed_output == s.output_payload_commitment`, a unit-output stage, and a
  malformed header are inadmissible.
- **Anyone defends** *(partly holds, 2026-10-29; see open question 6)*. A defence produced by a
  party holding only the public artifacts verifies identically to the claimer's.

  **The files are enough.** On `chain-example`, the claimer's run directory was hidden and the
  defence came from a bundle of:
  - `chain-commitment`;
  - `challenge.bin`, and `H_C`'s `commit.bin`;
  - the stage's `input_manifest.json`;
  - its input data: the chain's own external files, or the producer's `output.bin` and `.rindex`
    for a chained stage;
  - an `input.json` written by the defender, since it is not committed and holds absolute paths.

  From the same checkout path, `aggregate` (a chained stage) was defended and verified that way,
  with the same chain digest and `H_C` as the claimer's.

  **The checkout path is not.** From a second worktree (`/tmp/defender-raster`, same commit), the
  re-run and the walk succeed and find the mislabel. Proving then stops on `program identity
  drift`, on both `normalize` and `aggregate`. The tile ELFs embed the checkout path of `raster`
  and `raster-core` sources (panic locations), so the same source built elsewhere gets other
  image ids and another `program_commitment`. A defender who re-locks would fail `defend`'s
  `AnotherProgram` check instead.
- **Recur stage** *(done, 2026-10-29)*. A challenge on iris `prefill_range_l0` (2043 items, window
  128), whose terminal window opens mid-loop, can be defended both ways (S3 closed by
  `window-seed-reconstruction`).
  - In dev mode, every case `defend-verify`s:
    - `--forge claimed-output`: mislabelled, attesting `b20e1a1e…`;
    - `--forge item`: refuted;
    - `--forge pad` (2044 items): refuted;
    - `--forge truncate` (2042 items): refuted.

    The true challenge gets "no defence exists". Each defence takes about 25 s.
  - **Real GPU proofs** (RTX 3070, 2^18-cycle segments):
    - **Mislabel:** 1218 segments in 2359 s for the window. The defence guest took one
      segment (131,072 cycles) in 25 s. The real transition receipt was accepted by `prove()`
      as an assumption, so the manual resolve path is not needed. `defend-verify` passed with
      dev mode off.
    - **Refutation of a padded `H_C`** (2044 items; the length rule): 1218 segments in 2421 s,
      and the defence guest one segment in 26 s. `defend-verify` passed with dev mode off.
    - A dev-mode defence is refused with dev mode off ("proof is invalid").
- **Byte-identity** *(done, 2026-10-29)*. Honest `chain run` commitments and single-program
  `commit.bin`s are unchanged by every phase.

  At `cdab597` (before Phase 1) and at `8c81c47` (Phase 3), built in the same checkout, these are
  byte-identical:
  - `chain-example`'s `chain-commitment`, and each stage's `commit.bin`, `output.bin` and
    `input_manifest.json`;
  - `hello-tiles`' `commit.bin` (window 2);
  - all four freshly built `Raster.lock`s.

  So no phase moved a tile image id. The `chain-example` lock changes seen after Phase 3 were
  committed locks that were already stale.

## Decisions (2026-10-28)

1. **Bits per item: exact match** (was open question 6). `H_C`'s header must carry exactly the
   canonical `bits_packer` for its `window_size` — `FraudProofConfig::from_window_size`, the value
   every honest commitment uses — not merely a floor. The fingerprint's strength is then fixed by
   the window size, and a challenger cannot choose a weaker one.
2. **The state is named `TerminalMatch`** (was 4).
3. **`chain fraud-prove` folds into `chain challenge`** (was 5; first kept, then reversed the same
   day). Its `Link`/`Shape` receipts become the **proven** form of a challenge, and
   `fraud-verify` becomes `challenge-verify`. Its output-fraud evidence is dropped: it proved
   nothing about the claimer, and the disputed form replaces it.
4. **Data availability is local files for now** (was 7). `chain challenge` writes `H_C`'s
   `commit.bin` beside the challenge; `chain defend` reads it from there.
5. **Names.** Parties: claimer, challenger, defender. Commands: `chain challenge` /
   `challenge-verify`, `chain defend` / `defend-verify`; the defence's journal is `DefenceJournal`.
6. **One window size for the whole protocol: 128** (was open question 4). `H_C` must be committed
   at `protocol_window_size(fingerprint_len)` = 128, or for a trace of 128 items or fewer the largest
   power of two below its length (a commitment needs more items than its window; a strict 128 would
   leave every short stage — 4 to 14 items in `chain-example` — undisputable). Admission refuses any
   other window; `chain challenge` has no window flag. The required window is a function of `H_C`'s
   own header, so admission needs nothing else, and a challenger inflating `fingerprint_len` gains
   at most the cap. A trace of 2 items or fewer has no window and cannot be disputed
   (`TraceTooShort`). Commitment bodies are tens of KB at every measured size, so data
   availability gives no reason for larger windows. `PROTOCOL_WINDOW_SIZE` and
   `protocol_window_size` in `raster-prover/src/challenge.rs`.

## Measurements (2026-10-28)

iris `prefill_range_l0` as a one-stage chain, risc0 3.0.6, RTX 3070 8 GB, 2^18-cycle segments.
Terminal window = the mislabel proof's shape (stage output tampered in the commitment); mid-stage
window = the refutation's shape (`--fraud-step 757`). GPU times exclude the ~125 s first-proof
initialisation.

| | W = 2 | W = 16 | W = 128 |
|---|---|---|---|
| authenticated stage re-run + commit | 9 s | 9 s | 9 s |
| `H_C` body (`commit.bin`) | 20 KB | 7 KB | 40 KB |
| terminal window: segments | 14 | 102 | 1218 |
| terminal window: GPU prove | 8.9 s | 76 s | 2607 s |
| mid-stage window: segments | 17 | 164 | 1231 |
| mid-stage window: GPU prove | 10.7 s | 140 s | 2286 s |

Segment counts are exact (dev mode agrees). The W = 128 times are pessimistic: the per-segment
rate degraded within each long run (0.8 → 5.7 s/segment over 128 proofs, both runs, cause not
isolated), so at the fresh rate of ~0.8 s/segment the W = 128 windows are ~16 min.

**Open question 1 is answered: the window proof is not marginal.** Against a 9 s stage re-run, one
window proof costs about the same at W = 2, ~8–15× at W = 16, and ~100–290× at W = 128. That is the
condition chain-io named for preferring the cheap rule, so the cheap rule stands.

It also showed that **who pays was set by the challenger**: `H_C`'s window size was the
challenger's choice, and the defence's cost scales with it while the challenger's own cost
(re-run plus commit) does not. Decision 6 removes the choice.

At the protocol window, the same stage end to end (`chain challenge` on the tampered chain,
2026-10-28): trace of 2043 items → window 128; 21 s wall for the whole challenge; `challenge.bin`
385 bytes, `H_C`'s body 39 KB; admitted.

## Open questions

1. ~~Is the terminal proof marginal?~~ No — see §Measurements.
2. **Bond sizing.** The challenger's bond must cover at least one defence — an authenticated re-run
   plus one window proof — or false challenges become a cheap way to make honest claimers pay.
   With the window fixed (decision 6) the defence is at most one 128-item window per challenge, so
   the bound is per stage, not per challenger: on the measured stage ~1220 segments, ~16 GPU-minutes
   on the RTX 3070 at the fresh rate.
3. **Response window `T` against proof time.** *Left open (2026-10-28).* `T` must exceed the
   slowest stage's re-run plus one 128-item window proof, with margin for a defender that is not
   the claimer and for first-proof initialisation (~2 min here). Decision 6 bounds the proof; it
   does not choose `T`.
4. ~~Who chooses the window size?~~ The protocol — decision 6.
5. ~~**A `H_C` of the wrong length may be undefendable**~~ — closed 2026-10-29 by §Phase 3 plan,
   change 1, and checked end to end (`--forge pad`, `--forge truncate`). As found (2026-10-28): Completeness assumes every dishonest `H_C` diverges at some item. One with the honest
   items but a different `fingerprint_len` does not:
   - *padded* (honest items, then extra ones): the honest walk ends at `ProgramEnd` with no
     divergence, and that window is not `H_C`'s terminal one, so the guest refuses it — no refutation;
     the terminal window holds items the honest trace does not have — no mislabel proof;
   - *truncated*: every committed item matches, and the terminal window does not end in
     `ProgramEnd`, so it establishes no output — neither defence applies.
   Either way an honest claimer has no defence and is condemned at `T`. Likely fix, in Phase 3: the
   guest ties the trace's end to the commitment's — a window whose last item is `ProgramEnd` but
   that is not terminal, or a terminal window whose last item is not `ProgramEnd`, ends `Finished`
   (a refutation: `H_C` misstates the trace's length) — and `TraceVerifier` yields that window.
   Reproduce first with a padded and a truncated `H_C`.
6. **A defender must build at the claimer's checkout path** (found 2026-10-29, §Verification,
   "Anyone defends"). Tile image ids, and with them `program_commitment`, depend on the absolute
   source paths compiled into the tile ELFs. So "anyone may defend" holds only for a defender
   whose build reproduces the claimer's paths. The fix belongs to program identity, not to this
   protocol. Candidates are path-independent tile builds (`--remap-path-prefix` for the
   checkout and `$CARGO_HOME`, or risc0's Docker build) and a recorded, rebuildable build
   environment. Either changes every `program_commitment` once.
