# Proposal: `settlement-layer` — the clock, bonds and data availability a dispute settles on

Status: **proposed, deferred** (2026-10-29). Nothing is scheduled. This document records what
the settlement layer has to do, so that the protocol objects already built can be judged against
it. It is not a plan to build one now.

Updated 2026-10-30: what counts as an available body, and how to prove it where the body cannot
be read (§3); the v3 commitment body format, with revealed head and tail roots (§6).

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
awaiting body ──H_C's body available (§3)──► open(t0)              (the clock starts here, not at posting)
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
- **The clock starts when `H_C`'s body is available.** It does not start at posting
  (chain-io H7). Available means retrievable **and** accepted by `admit_body` (§3). A challenger
  who withholds the body, or posts one no defender can walk, never starts the clock, and so cannot
  win by silence.

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
- **`H_C`'s body**, `commit.bin`, named by the header in the challenge and addressed in the DA
  layer by `sha256` of its bytes. Its availability, defined below, starts the clock.
- **The challenged stage's committed inputs**: `input_manifest.json`, and the data it commits
  to. That is the chain's external inputs, or for a chained stage the producer's `output.bin` and
  `.rindex`.
- **The stage's program**: sources and `Raster.lock`, from which the defender rebuilds the tiles.

`input.json` is not in the list. It is a local file of paths, not a committed artifact, and each
defender writes their own. Today all of this is local files (`stage-challenge.md` decision 4).

#### What counts as an available body

`H_C`'s body is available for an admitted challenge when it is retrievable and
`raster_prover::challenge::admit_body(bytes, &admitted)` accepts it. That function is the one
definition. It requires the body to:
1. decode as a `TraceCommitment`;
2. be the commitment the challenge names: `header(body).digest() == H_C`;
3. be well-formed: `TraceCommitmentExt::validate()` passes. This is exactly what a defender's
   `TraceVerifier::new` requires before it walks the body.

The second condition does not imply the third. The header hashes the body's parts without
describing them: `revealed_tail_roots_commitment` hashes however many roots there are. So a
challenger can drop one tail root, recompute the header, and post a challenge that admission
accepts and whose body matches `H_C`. `chain defend` then refuses that body before producing any
proof. Reproduced 2026-10-30 with the test-only `chain challenge --forge drop-tail-root`.

A present but malformed body is therefore treated exactly like a withheld one.
`chain challenge-verify` and `chain defend` both apply `admit_body`, and `chain challenge` refuses
to write a body that fails it.

#### Proving availability where the body cannot be read

An on-chain contract cannot decode a body or run `admit_body`, so availability has to be proven
to it. A small **body guest** does this.

`B` is not part of the challenge `C`. `C` carries only `H_C`'s header, which commits to the parts
of `B` by hash (`fingerprint_root`, and the revealed-roots commitments of §6). `B` itself is
posted to the DA layer. So there are two commitments to `B`, for different readers:
- **`H_C`** is the protocol's identity for the commitment. It is structured so that guests can
  prove facts about parts of `B`, such as a fingerprint slice or a revealed root, without seeing
  all of it.
- **`sha256(B)`** is a flat hash of the bytes: the kind of commitment a DA layer can make about
  data it stores without interpreting.

No DA layer can compute `H_C`, since that means decoding `B` and rebuilding the header. The body
guest bridges the two.

**Statement.** Given public inputs `C` and `body_digest`, the receipt proves that the prover knew
bytes `B` with:
1. `sha256(B) == body_digest`;
2. `verify_challenge(C) == Ok(admitted)`;
3. `admit_body(B, admitted) == Ok(_)`.

**Journal.** `BodyAvailabilityJournal { challenge_digest, body_digest }`, where
`challenge_digest = sha256(postcard(C))`, the identity a defence already matches by. Admission runs
inside the guest on purpose. It decodes the whole chain commitment, which a contract cannot do
cheaply either. One receipt then says "admitted and available", and the contract never interprets
challenge bytes.

**What the contract checks** before opening the clock:
1. the receipt verifies against the pinned body-guest image id (§4);
2. its `challenge_digest` names a challenge posted on this chain commitment;
3. its `body_digest` is what the DA layer attests was posted.

`t0` is when the last of these lands. Without a receipt the clock never starts. So nobody ever has
to prove a body malformed, and no new defence kind is needed.

**Who proves it.** The challenger, the only party that gains from starting the clock. Anyone can,
from public data. An honest challenger always can, because `admit_body` accepts every well-formed
`H_C`: the check costs honest challengers some work but never blocks them.

**Binding `body_digest` to the DA layer.** The guest always commits `sha256(B)`, whatever the DA
layer is. An adapter for each DA layer links that to the layer's own commitment:
- **Hash-addressed DA**, or calldata hashed on-chain: direct equality, almost free.
- **EIP-4844 blobs**: the versioned hash commits to a KZG commitment, not to `sha256(B)`. The usual
  technique is a proof of equivalence. Derive `z = H(body_digest, versioned_hash)`; the guest
  evaluates `B`, read as the blob polynomial, at `z` to get `y` (scalar-field arithmetic only, no
  pairings); the contract checks `(z, y)` with the point-evaluation precompile. Cost not measured.
- **Celestia, Avail and similar**: a data-root inclusion proof (for example Blobstream), specific
  to each layer.

The proof says the body was posted. Keeping it retrievable until at least `t0 + T` is the DA
layer's job, not the proof's.

**Cost, estimated, not measured.** Linear in `|B|`: one postcard decode, SHA-256 over the revealed
parts of the body (32-byte roots under §6, not step records), the fingerprint-block root (about 32
blocks for iris at 1 bit per item), and one `fingerprint_value` check per revealed root. That is a
few segments, the order of the defence guest's 1 segment and far from a window proof's 1218.
On-chain use adds the Groth16 wrap, a fixed cost per receipt. Input should be read as postcard
frames, as the transition guest does, not with `env::read`.

**What has to move.** The body guest must run the same code as the host. Today that code is
host-only, in `raster-prover`:
- `trace.rs`: `TraceCommitmentExt::header` and `validate`, `fingerprint_blocks_root`,
  `fingerprint_block_leaf`, `FraudProofConfig::from_window_size` (which fixes the canonical bits
  per item), and the constants `MIN_BITS_PER_ITEM`, `MAX_BITS_PER_ITEM` and
  `MAX_FRAUD_PROOF_WINDOW_SIZE`;
- `challenge.rs`: `verify_challenge`, `protocol_window_size` and `admit_body`.

Their only host-only dependency is bridgetree's `TraceTree`, used by `fingerprint_blocks_root`. It
can be replaced by a fold with `combine_merkle_level` and `raster_core::merkle::EMPTY_ROOTS`. That
is the scheme the transition guest already uses to check block inclusion
(`assert_window_is_commitment_slice`): about 20 lines, and no new dependency. Hashing can use
`raster_core::hash::Sha256`, which `TraceCommitmentHeader::digest` already uses inside guests. The
error type moves too, or is mapped.

There are two places it could go:
- **`raster-core`** (recommended). Shared types already live there, because guests cannot depend
  on the prover, and the transition guest already links it. Any `raster-core` edit may move tile
  image ids, so every program is re-locked after the move.
- **A new small crate** that depends on `raster-core` and is linked by `raster-prover` and the body
  guest, but not by tiles. Later edits to it never move tile ids. The cost is one more crate.

Either way there is **one implementation**: `raster-prover`'s `header()` and `validate()` delegate
to the moved code rather than keep a copy. A host and a guest that disagreed about what is
well-formed would reintroduce this bug one layer down.

**Guarantees and limits.**
- **Soundness**: a receipt means `admit_body` passed, so `TraceVerifier::new` accepts the body, and
  a defender is never blocked by the body's structure.
- **Open caveat**: `validate()` covers what `TraceVerifier::new` checks. It is not yet shown that
  nothing later in `chain defend` rejects a body for a structural reason rather than a divergence.
  The four forge scenarios (`claimed-output`, `item`, `pad`, `truncate`) still defending is
  evidence, not proof, and the guest inherits whatever gap exists.
- **Not secret**: the body is public anyway. The receipt makes the check small for a contract; it
  hides nothing.
- **Guest upgrades**: the body-guest id joins the ids the contract pins (open question 5).

**Alternatives considered.**
- **The contract decodes and validates the body itself.** That is a second implementation in
  Solidity (postcard decoding, a depth-32 SHA Merkle tree, bit unpacking), which can drift from the
  Rust one, with gas growing with the body.
- **A defence that proves a body malformed.** It puts the burden on the defender, under the
  deadline, and needs a new defence kind.
- **A trusted arbiter or committee attests availability.** That brings back a trust assumption.

### 4. Verifying receipts where the settlement runs

The settlement layer verifies three kinds of receipt:
- a proven challenge's chain-fraud receipt;
- a defence receipt;
- a body-availability receipt, where the settlement cannot read the body (§3).

Both are risc0 receipts serialized with `postcard`. Spec `05` records that this is not a format
an on-chain verifier can consume. An on-chain settlement needs:
- the receipts wrapped for on-chain verification (risc0's succinct-to-Groth16 path);
- the guest image ids pinned in the contract: chain-fraud, defence, transition, authorization,
  and the body guest.

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

### 6. The body format: revealed head and tail roots (header v3)

`H_C`'s body is what gets posted to the DA layer and what the body guest decodes, so its format is
part of this layer. Today's format, v2:

```rust
TraceCommitment { fingerprint, revealed_items: Vec<StepRecord>, revealed_tail_roots: Vec<NodeHash> }
TraceCommitmentHeader {
    bits_packer, fingerprint_len, fingerprint_root,
    revealed_items_commitment, window_size, revealed_tail_roots_commitment,
}
```

The two revealed fields look like a head/tail pair, but they are not:

| | `revealed_items` | `revealed_tail_roots` |
|---|---|---|
| Which end | the first `window_size` items | the last `window_size` items |
| What | full `StepRecord`s | cumulative trace-tree roots, 32 bytes each |
| Read by | only its length, which defines `window_size`; the contents only enter `H_C` by hash | `validate()`, `TraceVerifier` (exact tail divergence), the transition guest (`final_committed_root`) |
| Held to the fingerprint | no | yes |

So most of a body is bytes nothing reads or checks. No guest opens `revealed_items_commitment`,
which `docs/issues/fraud-window-trace-ends.md` §4 already records. In `chain-example`'s 2,309-byte
body (window 8), about 2 KB are head step records; at window 128 the body carries 128 of them.

v3 replaces the head items with head roots, validated like the tail:

```rust
TraceCommitment {
    fingerprint: Fingerprint,
    window_size: u64,                    // explicit, no longer a count of revealed items
    revealed_head_roots: Vec<NodeHash>,  // roots at indices 0..w
    revealed_tail_roots: Vec<NodeHash>,  // roots at indices N-w..N, unchanged
}
TraceCommitmentHeader {
    bits_packer, fingerprint_len, fingerprint_root, window_size,
    revealed_head_roots_commitment,      // sha256(postcard(revealed_head_roots))
    revealed_tail_roots_commitment,      // unchanged
}
TRACE_COMMITMENT_DOMAIN = b"raster/trace-commitment/v3"
```

`validate()` gains:
- each root vector holds exactly `window_size` roots;
- each head root squeezes (`fingerprint_value`) to the fingerprint entry at its index, as tail
  roots already do;
- when `N < 2w` the two ranges overlap on indices `N-w..w`, and a root revealed by both must be the
  same.

**Nothing is lost.** The root at index `i` is the trace-tree root over the seed and items `0..=i`,
so it commits to every one of them. The last head root alone commits to all `w` head items. Head
roots bind exactly what head items bound. The only loss is readable step records, which nothing
reads, and unlike the items the roots are checked.

**What head roots buy is exact pinpointing, not detection.** A divergence at index `k` changes
every later root, so it escapes the fingerprint only with probability `2^(−b·(N−k))`. That is
negligible at the head, where `N − k ≥ N − w`. The tail needed roots because there `N − k ≤ w`,
down to a coin flip at 1 bit per item. Revealing roots at both ends is chosen for one uniform
rule: a window may end on a divergence proven by revealed root, at either end of the trace. The
smaller alternative, dropping the head entirely and keeping an explicit `window_size`, was
considered and not chosen.

**How the prover's window choice and proof change:**
- **`TraceVerifier`**: `revealed_tail_root_at` becomes `revealed_root_at`, covering both ranges. A
  head divergence is located at its true index, which can end the window earlier than the first
  fingerprint entry that differs. Both windows are valid.
- **Head windows open at index 0.** A window ending at `j < w` cannot hold `w` items, and only a
  window opening at trace index 0 may be short. These are the genesis-opening windows
  `assert_opens_at_genesis` already pins, so root divergence at the head needs no new margin rule.
- **Transition guest**: `TransitionInput` gains `revealed_head_roots` beside `revealed_tail_roots`.
  Both are read only at `Init` and bound against their header commitments, with the count check.
  `final_committed_root` comes from whichever range holds the window's final index; the host
  supplies only that range. `finalize` and `window_end` do not change.
- **Admission** is unchanged: it reads `window_size`, `fingerprint_len` and `bits_packer` from the
  header, which v3 keeps.
- **Mislabel and terminal windows** are unaffected.

**Size.** A v3 body is about the fingerprint plus `2·w·32` bytes: about 0.56 KB for `chain-example`
(window 8) against 2.3 KB today, and about 8.5 KB at window 128 for iris (2043 items) against tens
of KB. This is what the DA layer stores per challenge, and what the body guest decodes.

**Migration.** It is a format break, best landed before any body is posted to a DA layer:
- every `commit.bin`, every `H_C` and `refuted_trace_commitment`, and every stored challenge and
  defence change;
- the transition guest changes, so its image id, which relying parties pin, moves;
- the types live in `raster-core`, so tile image ids may move: re-lock every program and
  regenerate commitments.

## What could be built in this repository first

None of this needs the settlement layer itself, and none of it is planned now:
- A pure `settle(challenge, defences, body_available_at, now, T) -> Pending | Dismissed | Condemned`
  over the existing objects, with unit tests for every transition above. A contract would then
  only supply the clock, the bonds and the receipt verifier.
- A local `chain settle` command applying it to the files on disk, with an explicit `--now`, for
  end-to-end tests.
- The v3 body format (§6).
- The move of `header()`, `validate()` and admission into guest-buildable code (§3), then the
  body guest and its journal.

Already built (2026-10-30): `admit_body`, applied by `chain challenge`, `chain challenge-verify`
and `chain defend` (§3).

## Dependencies

- `stage-challenge.md` Phases 1–3: **done**.
- Path-independent program identity (`stage-challenge.md` open question 6): needed for any
  defender other than the claimer.
- A portable, on-chain-verifiable receipt format (spec `05`): needed for an on-chain settlement;
  not needed for an off-chain arbiter.
- A data-availability layer: needed before `T` can start for anyone but a local defender.
- The v3 body format (§6): needed before any body is posted to a DA layer, since it changes every
  body and every `H_C`.
- The body guest (§3): needed for an on-chain settlement; an off-chain arbiter can run
  `admit_body` directly.

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
7. **Binding to EIP-4844 blobs**: what the proof of equivalence costs in the body guest (§3).
8. **Where the moved code lives**: `raster-core`, or a separate guest-buildable crate that tiles
   do not link (§3).
9. **A challenge whose body never becomes available**: does it expire, and is the challenger's
   bond returned or forfeited?
