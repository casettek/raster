# Proposal: `settlement-layer` — the clock, bonds and data availability a dispute settles on

Status: **proposed, deferred** (2026-10-29). Nothing is scheduled. This document records what
the settlement layer has to do, so that the protocol objects already built can be judged against
it. It is not a plan to build one now.

Parties are the same as in [`stage-challenge.md`](./stage-challenge.md): the **claimer** publishes a
chain commitment, a **challenger** challenges it, and a **defender** (the claimer, or anyone)
defends against a disputed challenge.

Builds on:
- [`stage-challenge.md`](./stage-challenge.md) (Phases 1–3 implemented). Its Phase 4, "the
  timeout, specified not built", is this proposal.
- [`chain-io-commitment.md`](./chain-io-commitment.md) §Assumed infrastructure. Its three
  assumed pieces — a settlement contract with a clock, data availability for artifacts, and
  bonding — are what this document specifies. Attacks marked **[infra]** there (H7, H8, S4) are
  closed here, not in the proofs.
- [`docs/specs/core/4-verify/05-on-chain-interface-assumptions.md`](../specs/core/4-verify/05-on-chain-interface-assumptions.md),
  which records that the toolchain has no on-chain client, contract bindings or portable receipt
  format today.

## Why it is needed

A disputed challenge carries no proof (the cheap-challenge rule, `stage-challenge.md` §Why the
cheap rule). It condemns a chain only by **going unanswered**. Everything up to that point is built:
- a challenge can be posted and admitted (`chain challenge`, `chain challenge-verify`);
- a defence can be produced and verified (`chain defend`, `chain defend-verify`).

What is missing is the step that turns "admitted, awaiting a defence" into a verdict. That step
needs a clock everyone agrees on, and money at stake on both sides. Those cannot live in this
repository. Until they exist, a disputed challenge is evidence, not a verdict. A proven challenge
(`Link`, `Shape`) does not need any of this: verifying its receipt condemns the chain outright.

## The decision rule

For one chain commitment `D` (named by its digest), each challenge moves through these states:

```text
posted ──verify() refuses──────────────────────────► rejected      (never admitted; no bond taken)
posted ──Proven, receipt verifies──────────────────► condemned     (no clock)
posted ──Disputed, admitted──► awaiting body
awaiting body ──H_C's body retrievable──► open(t0)                 (the clock starts here, not at posting)
open(t0) ──verified DefenceJournal, now < t0 + T──► dismissed      (challenger's bond forfeited)
open(t0) ──now ≥ t0 + T, no verified defence──────► condemned     (stage challenged_stage to blame)
```

- **Admission is the existing pure function.** `raster_prover::challenge::verify` decides `rejected`
  against admitted, with no state. A settlement layer runs it at posting, so an inadmissible
  challenge never takes a bond or starts a clock.
- **A defence matches by digest.** It counts for a challenge only if
  `DefenceJournal::challenge_digest == sha256(postcard(StageChallenge))` of that challenge, and if
  `verify_defence` accepts its receipt (the pinned defence image id, plus the known-good
  transition and authorization ids). The first verified defence wins, and later ones are moot.
- **Dismissal is about the challenge, not the stage.** A dismissed challenge shows that *that*
  `H_C` and claimed output were false. It says nothing about the stage's output. Another challenger
  may challenge the same stage with another `H_C`, and gets its own clock.
- **Condemnation is about the chain.** One condemned challenge condemns `D`, with
  `challenged_stage` to blame. Any challenges still open on `D` become moot.
- **The clock starts when `H_C`'s body is retrievable.** It does not start at posting
  (chain-io H7). A challenger who withholds the body never starts the clock, and so cannot win by
  silence.

## Components

### 1. The clock and the response window `T`

`T` must cover what the slowest honest defender needs:
- an authenticated re-run of the challenged stage;
- one window proof at the protocol window (128, `stage-challenge.md` decision 6);
- the defence aggregation;
- margin, for first-proof initialisation and for a defender who is not the claimer.

Measured on iris `prefill_range_l0` (2043 items), RTX 3070 8 GB, risc0 3.0.6, 2^18-cycle segments
(`stage-challenge.md` §Verification, 2026-10-29):

| step | cost |
|---|---|
| authenticated stage re-run | ~9 s |
| window proof at 128 (mislabel) | 1218 segments, 2359 s |
| window proof at 128 (refutation, padded `H_C`) | 1218 segments, 2421 s |
| defence aggregation | 1 segment, ~25 s |
| CUDA initialisation, first proof per process | ~125 s |

At the fresh rate (~0.8 s/segment) the window is about 16 minutes. In practice the per-segment
rate degrades over a long run, so measured runs take about 40 minutes. `T` is a per-deployment
parameter: it must be set from the slowest stage the chain admits, on the slowest hardware a
defender is assumed to have. **Open:** whether `T` is global, or per chain from the slowest stage.

### 2. Bonds

- **The challenger's bond** must at least cover one defence: the re-run, one window proof and the
  aggregation above. Otherwise a false challenge is a cheap way to make honest claimers pay
  (chain-io H8). Under the cheap rule this bond is not optional; it is what prices a false
  challenge. It is forfeited on dismissal.
- **The claimer's bond** is what condemnation costs the claimer. Without it, condemnation has
  no consequence beyond the chain being marked.
- **Open:** bond sizes; whether the forfeited challenger bond pays the defender (who did the work,
  and may not be the claimer) or is burned; whether a defender must post anything at all (today
  a defence is self-certifying, so no).

### 3. Data availability

During the response window the following must be retrievable by any defender. The list was
established by running a defence with the claimer's run directory hidden
(`stage-challenge.md` §Verification, "Anyone defends"):
- **The challenge**: `challenge.bin`, which carries the chain commitment bytes.
- **`H_C`'s body**, `commit.bin`, content-addressed by the header in the challenge. Its
  availability starts the clock.
- **The challenged stage's committed inputs**: `input_manifest.json`, and the data it commits
  to. That is the chain's external inputs, or for a chained stage the producer's `output.bin` and
  `.rindex`.
- **The stage's program**: sources and `Raster.lock`, from which the defender rebuilds the tiles.

`input.json` is not in the list. It is a local file of paths, not a committed artifact, and each
defender writes their own. Today all of this is local files (`stage-challenge.md` decision 4).

### 4. Verifying receipts where the settlement runs

The settlement layer verifies two kinds of receipt:
- a proven challenge's chain-fraud receipt;
- a defence receipt.

Both are risc0 receipts serialized with `postcard`. Spec `05` records that this is not a format
an on-chain verifier can consume. An on-chain settlement needs:
- the receipts wrapped for on-chain verification (risc0's succinct-to-Groth16 path);
- the guest image ids pinned in the contract: chain-fraud, defence, transition, authorization.

The guests already commit the inner image ids they verified against, so that a relying party can
pin them (`ChainFraudJournal`, `DefenceJournal`). **Open:** how ids are updated when a guest
changes, since every guest edit moves its id.

### 5. Defender liveness

The soundness of the cheap rule relies on at least one honest defender answering within `T`. An
offline claimer is protected only if *someone else* can defend. Today that holds only for a
defender who builds the stage's tiles at the claimer's checkout path: tile image ids depend on the
absolute source paths in the tile ELFs (`stage-challenge.md` open question 6). **This is a
prerequisite for the settlement layer, not part of it.** Until program identity is
path-independent, `T` protects only claimers who stay online.

## What could be built in this repository first

None of this needs the settlement layer itself, and none of it is planned now:
- A pure `settle(challenge, defences, body_available_at, now, T) -> Pending | Dismissed | Condemned`
  over the existing objects, with unit tests for every transition above. A contract would then
  only supply the clock, the bonds and the receipt verifier.
- A local `chain settle` command applying it to the files on disk, with an explicit `--now`, for
  end-to-end tests.

## Dependencies

- `stage-challenge.md` Phases 1–3: **done**.
- Path-independent program identity (`stage-challenge.md` open question 6): needed for any
  defender other than the claimer.
- A portable, on-chain-verifiable receipt format (spec `05`): needed for an on-chain settlement;
  not needed for an off-chain arbiter.
- A data-availability layer: needed before `T` can start for anyone but a local defender.

## Open questions

1. **`T`**: one global value, or one per chain from its slowest stage? How much margin, and for
   which defender hardware?
2. **Bond sizes**, and who receives a forfeited challenger bond: the defender, or nobody?
3. **Concurrent challenges** on one chain: is there a cap per stage, or per challenger? Bonds
   price each one, but a defender's capacity is finite within `T`.
4. **The DA layer**: what holds the bodies, for how long after `T`, and who pays for it?
5. **Guest upgrades**: how pinned image ids change without voiding challenges already open.
6. **On-chain or off-chain arbiter**: on-chain needs receipt wrapping (component 4); an
   off-chain arbiter needs trust in whoever runs the clock.
